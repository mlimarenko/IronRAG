use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};

use anyhow::Context;
use futures::TryStreamExt;
use sqlx::PgConnection;
use tokio::time::{Instant, timeout_at};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::graph_quality::GraphSummaryConfidenceStatus,
    infra::repositories::{
        self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow, RuntimeGraphSqlDeadline,
    },
    services::graph::{error::GraphServiceError, projection::active_projection_version},
};

#[derive(Debug, Clone, Default)]
pub struct GraphSummaryService;

const INLINE_SUMMARY_REFRESH_TARGET_LIMIT: usize = 500;
const INLINE_SUMMARY_REFRESH_BUDGET: Duration = Duration::from_secs(5);

/// Summary work derived from a graph projection but not yet authorized by a
/// lifecycle publication. The source-truth generation is deliberately absent:
/// callers may bind it only after the lifecycle transaction commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGraphSummaryRefresh {
    pub node_ids: Vec<Uuid>,
    pub edge_ids: Vec<Uuid>,
    pub broad_refresh: bool,
}

impl PendingGraphSummaryRefresh {
    #[must_use]
    pub const fn broad() -> Self {
        Self { node_ids: Vec::new(), edge_ids: Vec::new(), broad_refresh: true }
    }

    #[must_use]
    pub fn targeted(mut node_ids: Vec<Uuid>, mut edge_ids: Vec<Uuid>) -> Self {
        node_ids.sort_unstable();
        node_ids.dedup();
        edge_ids.sort_unstable();
        edge_ids.dedup();
        Self { node_ids, edge_ids, broad_refresh: false }
    }

    #[must_use]
    pub const fn is_targeted(&self) -> bool {
        !self.broad_refresh && (!self.node_ids.is_empty() || !self.edge_ids.is_empty())
    }

    /// Coalesces publication work. A broad request dominates; otherwise the
    /// exact target sets are unioned deterministically.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        if self.broad_refresh || other.broad_refresh {
            return Self::broad();
        }
        let node_ids = self.node_ids.into_iter().chain(other.node_ids).collect();
        let edge_ids = self.edge_ids.into_iter().chain(other.edge_ids).collect();
        Self::targeted(node_ids, edge_ids)
    }

    #[must_use]
    fn bind_source_truth_version(&self, source_truth_version: i64) -> GraphSummaryRefreshRequest {
        GraphSummaryRefreshRequest {
            source_truth_version,
            node_ids: self.node_ids.clone(),
            edge_ids: self.edge_ids.clone(),
            broad_refresh: self.broad_refresh,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedGraphSummaryRefreshOutcome {
    Applied { invalidated: u64, refreshed: u64 },
    GenerationMismatch { observed_source_truth_version: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphSummaryRefreshRequest {
    source_truth_version: i64,
    node_ids: Vec<Uuid>,
    edge_ids: Vec<Uuid>,
    broad_refresh: bool,
}

impl GraphSummaryRefreshRequest {
    #[must_use]
    const fn is_targeted(&self) -> bool {
        !self.broad_refresh && (!self.node_ids.is_empty() || !self.edge_ids.is_empty())
    }
}

fn log_graph_summary_refresh_stale(
    library_id: Uuid,
    expected_source_truth_version: i64,
    observed_source_truth_version: i64,
) {
    tracing::warn!(
        %library_id,
        expected_source_truth_version,
        observed_source_truth_version,
        "skipping active graph summary refresh because a newer lifecycle publication committed",
    );
}

fn log_graph_summary_refresh_rollback_failure(library_id: Uuid, rollback_error: &sqlx::Error) {
    tracing::warn!(
        %library_id,
        ?rollback_error,
        "failed to release graph summary source-generation guard after refresh error",
    );
}

fn log_graph_summary_refresh_skipped(
    library_id: Uuid,
    affected_targets: usize,
    refresh: &GraphSummaryRefreshRequest,
) {
    tracing::info!(
        %library_id,
        candidate_targets_lower_bound = affected_targets,
        inline_limit = INLINE_SUMMARY_REFRESH_TARGET_LIMIT,
        targeted = refresh.is_targeted(),
        broad = refresh.broad_refresh,
        "skipping inline active graph summary generation: candidate volume exceeds the inline limit (not an admitted count)",
    );
}

impl GraphSummaryService {
    /// Applies deferred summary work only while the lifecycle publication's
    /// source generation is still current. `FOR SHARE` holds the parent row
    /// stable for the whole refresh, so a concurrent publisher either commits
    /// first (and causes a safe mismatch) or waits until this refresh releases
    /// the guard. Lifecycle state is never changed by this post-commit helper.
    pub async fn apply_published_refresh(
        &self,
        state: &AppState,
        library_id: Uuid,
        expected_source_truth_version: i64,
        pending: &PendingGraphSummaryRefresh,
    ) -> Result<PublishedGraphSummaryRefreshOutcome, GraphServiceError> {
        let deadline = Instant::now() + INLINE_SUMMARY_REFRESH_BUDGET;
        let sql_deadline = RuntimeGraphSqlDeadline::at(deadline);
        let mut generation_guard = timeout_at(deadline, state.persistence.postgres.begin())
            .await
            .context("graph summary deadline exhausted while acquiring transaction")?
            .context("failed to begin graph summary source-generation guard")?;

        // The transaction outlives the cancellable borrowed work so work and
        // precommit failures, including lock timeout, are explicitly rolled back.
        let apply_result = timeout_at(deadline, async {
            sql_deadline.install(&mut generation_guard).await?;
            let generation = sqlx::query_as::<_, (i64, Uuid)>(
                "select coalesce(source_truth_version, 1), workspace_id
                 from catalog_library
                 where id = $1
                 for share",
            )
            .bind(library_id)
            .fetch_optional(&mut *generation_guard)
            .await
            .context("failed to lock graph summary source generation")?
            .ok_or(GraphServiceError::LibraryNotFound { library_id })?;
            let observed_source_truth_version = generation.0.max(1);
            let workspace_id = generation.1;

            if observed_source_truth_version != expected_source_truth_version {
                log_graph_summary_refresh_stale(
                    library_id,
                    expected_source_truth_version,
                    observed_source_truth_version,
                );
                return Ok(PublishedGraphSummaryRefreshOutcome::GenerationMismatch {
                    observed_source_truth_version,
                });
            }

            let refresh = pending.bind_source_truth_version(expected_source_truth_version);
            self.apply_summary_refresh_locked(
                &mut generation_guard,
                workspace_id,
                library_id,
                &refresh,
                &sql_deadline,
            )
            .await
        })
        .await
        .context("graph summary operation deadline exhausted")
        .map_err(GraphServiceError::from)
        .and_then(std::convert::identity);

        // Revalidate the remaining budget before COMMIT. Transaction-local
        // caps disappear on commit/rollback, restoring the session setting.
        let apply_result = match apply_result {
            Ok(outcome @ PublishedGraphSummaryRefreshOutcome::Applied { .. }) => {
                match timeout_at(deadline, sql_deadline.install(&mut generation_guard)).await {
                    Ok(Ok(_)) if Instant::now() < deadline => Ok(outcome),
                    Ok(Ok(_)) => {
                        Err(anyhow::anyhow!("graph summary deadline exhausted before commit")
                            .into())
                    }
                    Ok(Err(error)) => Err(anyhow::Error::from(error).into()),
                    Err(error) => Err(anyhow::Error::from(error).into()),
                }
            }
            result => result,
        };

        match apply_result {
            Ok(outcome @ PublishedGraphSummaryRefreshOutcome::GenerationMismatch { .. }) => {
                generation_guard
                    .rollback()
                    .await
                    .context("failed to release stale graph summary source-generation guard")?;
                Ok(outcome)
            }
            Ok(outcome) => {
                // COMMIT retains the remaining server cap. SQLx consumes the
                // transaction here and queues rollback on drop if commit fails;
                // network completion and cleanup are not a five-second SLA.
                generation_guard
                    .commit()
                    .await
                    .context("failed to release graph summary source-generation guard")?;
                Ok(outcome)
            }
            Err(error) => {
                if let Err(rollback_error) = generation_guard.rollback().await {
                    log_graph_summary_refresh_rollback_failure(library_id, &rollback_error);
                }
                Err(error)
            }
        }
    }

    /// Applies invalidation and the conditional inline refresh on the held guard.
    async fn apply_summary_refresh_locked(
        &self,
        connection: &mut PgConnection,
        workspace_id: Uuid,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
        deadline: &RuntimeGraphSqlDeadline,
    ) -> Result<PublishedGraphSummaryRefreshOutcome, GraphServiceError> {
        let invalidated =
            self.invalidate_summaries(connection, library_id, refresh, deadline).await?;
        if !refresh.broad_refresh && !refresh.is_targeted() {
            return Ok(PublishedGraphSummaryRefreshOutcome::Applied { invalidated, refreshed: 0 });
        }
        deadline.install(connection).await.context("failed to budget summary snapshot read")?;
        let snapshot =
            repositories::get_runtime_graph_snapshot_with_executor(&mut *connection, library_id)
                .await
                .context("failed to load runtime graph snapshot for summary refresh sizing")?;
        if refresh.broad_refresh && snapshot.is_none() {
            return Ok(PublishedGraphSummaryRefreshOutcome::Applied { invalidated, refreshed: 0 });
        }
        let projection_version = active_projection_version(snapshot.as_ref());
        let targets = if refresh.broad_refresh {
            deadline
                .install(connection)
                .await
                .context("failed to budget summary candidate probe")?;
            let candidates = repositories::probe_runtime_graph_summary_targets_with_executor(
                &mut *connection,
                library_id,
                projection_version,
                (INLINE_SUMMARY_REFRESH_TARGET_LIMIT + 1) as i64,
            )
            .await
            .context("failed to probe graph summary candidate identities")?;
            let (nodes, edges): (Vec<_>, Vec<_>) =
                candidates.into_iter().partition(|(kind, _)| kind == "node");
            PendingGraphSummaryRefresh::targeted(
                nodes.into_iter().map(|(_, id)| id).collect(),
                edges.into_iter().map(|(_, id)| id).collect(),
            )
            .bind_source_truth_version(refresh.source_truth_version)
        } else {
            refresh.clone()
        };
        let affected_targets = targets.node_ids.len().saturating_add(targets.edge_ids.len());
        let refreshed =
            if self.should_batch_refresh(affected_targets, INLINE_SUMMARY_REFRESH_TARGET_LIMIT) {
                Self::refresh_summaries(
                    &mut *connection,
                    workspace_id,
                    library_id,
                    &targets,
                    projection_version,
                    deadline,
                )
                .await?
            } else {
                if affected_targets > 0 {
                    log_graph_summary_refresh_skipped(library_id, affected_targets, refresh);
                }
                0
            };
        Ok(PublishedGraphSummaryRefreshOutcome::Applied { invalidated, refreshed })
    }

    async fn invalidate_summaries(
        &self,
        connection: &mut PgConnection,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
        deadline: &RuntimeGraphSqlDeadline,
    ) -> Result<u64, GraphServiceError> {
        let source_truth_version = refresh.source_truth_version;

        if refresh.is_targeted() {
            deadline
                .install(connection)
                .await
                .context("failed to budget targeted summary invalidation")?;
            Ok(repositories::supersede_runtime_graph_summaries_for_targets_with_executor(
                &mut *connection,
                library_id,
                source_truth_version,
                &refresh.node_ids,
                &refresh.edge_ids,
            )
            .await
            .context(
                "failed to supersede targeted active graph summaries after source-truth change",
            )?)
        } else if refresh.broad_refresh {
            deadline
                .install(connection)
                .await
                .context("failed to budget library summary invalidation")?;
            Ok(repositories::supersede_runtime_graph_summaries_for_library_with_executor(
                &mut *connection,
                library_id,
                source_truth_version,
            )
            .await
            .context(
                "failed to supersede library active graph summaries after source-truth change",
            )?)
        } else {
            Ok(0)
        }
    }

    #[must_use]
    const fn should_batch_refresh(&self, affected_targets: usize, batch_limit: usize) -> bool {
        affected_targets > 0 && affected_targets <= batch_limit
    }

    async fn refresh_summaries(
        connection: &mut PgConnection,
        workspace_id: Uuid,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
        projection_version: i64,
        deadline: &RuntimeGraphSqlDeadline,
    ) -> Result<u64, GraphServiceError> {
        let source_truth_version = refresh.source_truth_version;

        deadline.install(connection).await.context("failed to budget summary node hydration")?;
        let nodes = repositories::list_admitted_runtime_graph_nodes_by_ids_with_executor(
            &mut *connection,
            library_id,
            projection_version,
            &refresh.node_ids,
        )
        .await
        .context("failed to hydrate summary nodes")?;
        deadline.install(connection).await.context("failed to budget summary edge hydration")?;
        let edges = repositories::list_admitted_runtime_graph_edges_by_ids_with_executor(
            &mut *connection,
            library_id,
            projection_version,
            &refresh.edge_ids,
        )
        .await
        .context("failed to hydrate summary edges")?;
        let node_label_index = load_summary_node_labels(
            connection,
            library_id,
            projection_version,
            &nodes,
            &edges,
            deadline,
        )
        .await?;

        let mut refreshed = 0u64;
        for node in &nodes {
            let summary_fragments =
                load_summary_fragments(&node.metadata_json, node.summary.as_deref());
            let evidence = load_evidence_fragments_if_needed(
                connection,
                library_id,
                ("node", node.id),
                &summary_fragments,
                2,
                deadline,
            )
            .await?;
            let (summary_text, confidence_status, warning_text) =
                build_node_summary(node, &summary_fragments, &evidence);
            deadline.install(connection).await.context("failed to budget node summary write")?;
            repositories::upsert_runtime_graph_target_summary_with_connection(
                connection,
                &repositories::UpsertRuntimeGraphTargetSummaryInput {
                    workspace_id,
                    library_id,
                    target_kind: "node".to_string(),
                    target_id: node.id,
                    summary_text,
                    confidence_status,
                    support_count: node.support_count,
                    source_truth_version,
                    generated_from_mutation_id: None,
                    warning_text,
                },
            )
            .await
            .with_context(|| {
                format!("failed to upsert active graph summary for node {}", node.id)
            })?;
            refreshed += 1;
        }

        for edge in &edges {
            let summary_fragments =
                load_summary_fragments(&edge.metadata_json, edge.summary.as_deref());
            let evidence = load_evidence_fragments_if_needed(
                connection,
                library_id,
                ("edge", edge.id),
                &summary_fragments,
                1,
                deadline,
            )
            .await?;
            let from_label = node_label_index
                .get(&edge.from_node_id)
                .cloned()
                .unwrap_or_else(|| edge.from_node_id.to_string());
            let to_label = node_label_index
                .get(&edge.to_node_id)
                .cloned()
                .unwrap_or_else(|| edge.to_node_id.to_string());
            let (summary_text, confidence_status, warning_text) =
                build_edge_summary(edge, &from_label, &to_label, &summary_fragments, &evidence);
            deadline.install(connection).await.context("failed to budget edge summary write")?;
            repositories::upsert_runtime_graph_target_summary_with_connection(
                connection,
                &repositories::UpsertRuntimeGraphTargetSummaryInput {
                    workspace_id,
                    library_id,
                    target_kind: "edge".to_string(),
                    target_id: edge.id,
                    summary_text,
                    confidence_status,
                    support_count: edge.support_count,
                    source_truth_version,
                    generated_from_mutation_id: None,
                    warning_text,
                },
            )
            .await
            .with_context(|| {
                format!("failed to upsert active graph summary for edge {}", edge.id)
            })?;
            refreshed += 1;
        }

        Ok(refreshed)
    }
}

async fn load_summary_node_labels(
    connection: &mut PgConnection,
    library_id: Uuid,
    projection_version: i64,
    nodes: &[RuntimeGraphNodeRow],
    edges: &[RuntimeGraphEdgeRow],
    deadline: &RuntimeGraphSqlDeadline,
) -> anyhow::Result<HashMap<Uuid, String>> {
    let mut labels =
        nodes.iter().map(|node| (node.id, node.label.clone())).collect::<HashMap<_, _>>();
    let missing_endpoint_ids = edges
        .iter()
        .flat_map(|edge| [edge.from_node_id, edge.to_node_id])
        .filter(|node_id| !labels.contains_key(node_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if !missing_endpoint_ids.is_empty() {
        deadline
            .install(connection)
            .await
            .context("failed to budget summary endpoint hydration")?;
        let endpoint_nodes = repositories::list_admitted_runtime_graph_nodes_by_ids_with_executor(
            &mut *connection,
            library_id,
            projection_version,
            &missing_endpoint_ids,
        )
        .await
        .context("failed to load endpoint nodes for normalized edge summaries")?;
        labels.extend(endpoint_nodes.into_iter().map(|node| (node.id, node.label)));
    }
    Ok(labels)
}

async fn load_evidence_fragments_if_needed(
    connection: &mut PgConnection,
    library_id: Uuid,
    target: (&str, Uuid),
    summary_fragments: &[String],
    limit: usize,
    deadline: &RuntimeGraphSqlDeadline,
) -> anyhow::Result<Vec<String>> {
    let (target_kind, target_id) = target;
    if !summary_fragments.is_empty() {
        return Ok(Vec::new());
    }
    deadline.install(connection).await.context("failed to budget summary evidence stream")?;
    let mut evidence = repositories::stream_runtime_graph_evidence_text_by_target_with_executor(
        &mut *connection,
        library_id,
        target_kind,
        target_id,
    );
    let mut fragments = BTreeSet::new();
    while let Some(text) = evidence.try_next().await.with_context(|| {
        format!("failed to stream summary evidence for {target_kind} {target_id}")
    })? {
        retain_evidence_fragment(&mut fragments, &text, limit);
        // A ready row stream must still let the enclosing operation timer run.
        tokio::task::consume_budget().await;
    }
    Ok(fragments.into_iter().collect())
}

fn build_node_summary(
    node: &RuntimeGraphNodeRow,
    summary_fragments: &[String],
    evidence_fragments: &[String],
) -> (String, String, Option<String>) {
    let lead = summary_fragments.first().cloned().or_else(|| evidence_fragments.first().cloned());
    let support_sentence = if node.support_count > 1 {
        Some(format!("Supported by {} active evidence lines.", node.support_count))
    } else {
        None
    };
    let fallback = if node.node_type == "document" {
        format!("{} is a source document node in the active library graph.", node.label)
    } else {
        format!("{} is an active {} in the library graph.", node.label, node.node_type)
    };
    let summary_text =
        compose_summary_text(lead.as_deref(), support_sentence.as_deref(), &fallback);
    let (confidence_status, warning_text) = classify_summary_confidence(
        node.support_count,
        &node.metadata_json,
        summary_fragments.len(),
    );
    (summary_text, confidence_status, warning_text)
}

fn build_edge_summary(
    edge: &RuntimeGraphEdgeRow,
    from_label: &str,
    to_label: &str,
    summary_fragments: &[String],
    evidence_fragments: &[String],
) -> (String, String, Option<String>) {
    let relationship_sentence =
        format!("{} {} {}.", from_label, edge.relation_type.replace('_', " "), to_label);
    let lead = summary_fragments
        .first()
        .cloned()
        .or_else(|| evidence_fragments.first().cloned())
        .unwrap_or(relationship_sentence);
    let support_sentence = if edge.support_count > 1 {
        Some(format!("Supported by {} active relationship evidence lines.", edge.support_count))
    } else {
        None
    };
    let summary_text = compose_summary_text(Some(&lead), support_sentence.as_deref(), &lead);
    let (confidence_status, warning_text) = classify_summary_confidence(
        edge.support_count,
        &edge.metadata_json,
        summary_fragments.len(),
    );
    (summary_text, confidence_status, warning_text)
}

fn load_summary_fragments(
    metadata: &serde_json::Value,
    fallback_summary: Option<&str>,
) -> Vec<String> {
    let mut values = metadata
        .get("summary_fragments")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(normalize_fragment)
        .collect::<BTreeSet<_>>();

    if let Some(fallback_summary) = fallback_summary.and_then(normalize_fragment) {
        values.insert(fallback_summary);
    }

    values.into_iter().collect()
}

fn retain_evidence_fragment(fragments: &mut BTreeSet<String>, text: &str, limit: usize) {
    if limit == 0 {
        return;
    }
    if let Some(fragment) = normalize_fragment(text) {
        fragments.insert(fragment);
        if fragments.len() > limit {
            fragments.pop_last();
        }
    }
}

fn normalize_fragment(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }

    let truncated = if normalized.chars().count() > 220 {
        let mut shortened = normalized.chars().take(217).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        normalized
    };
    Some(ensure_sentence(&truncated))
}

fn compose_summary_text(
    lead: Option<&str>,
    support_sentence: Option<&str>,
    fallback: &str,
) -> String {
    let mut parts = Vec::new();
    parts.push(
        lead.map(ensure_sentence)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| ensure_sentence(fallback)),
    );
    if let Some(support_sentence) = support_sentence.map(ensure_sentence)
        && !parts.iter().any(|part| part == &support_sentence)
    {
        parts.push(support_sentence);
    }
    parts.join(" ")
}

fn ensure_sentence(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if matches!(trimmed.chars().last(), Some('.' | '!' | '?')) {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn classify_summary_confidence(
    support_count: i32,
    metadata: &serde_json::Value,
    summary_fragment_count: usize,
) -> (String, Option<String>) {
    let has_partial_support =
        metadata.get("has_partial_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_failed_support =
        metadata.get("has_failed_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_recovered_support =
        metadata.get("has_recovered_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_conflict = summary_fragment_count > 1;

    let status = if has_conflict {
        GraphSummaryConfidenceStatus::Conflicted
    } else if has_failed_support {
        GraphSummaryConfidenceStatus::Weak
    } else if has_partial_support || has_recovered_support || support_count == 2 {
        GraphSummaryConfidenceStatus::Partial
    } else if support_count >= 3 {
        GraphSummaryConfidenceStatus::Strong
    } else {
        GraphSummaryConfidenceStatus::Weak
    };

    let warning = if has_conflict {
        Some("Active evidence still carries differing summary fragments.".to_string())
    } else if has_failed_support {
        Some(
            "Some supporting extraction attempts failed, so the summary is conservative."
                .to_string(),
        )
    } else if has_partial_support || has_recovered_support {
        Some("Summary includes recovered or partial extraction support.".to_string())
    } else if support_count <= 1 {
        Some("Summary is supported by a single active evidence line.".to_string())
    } else {
        None
    };

    (
        serde_json::to_string(&status)
            .unwrap_or_else(|_| "\"weak\"".to_string())
            .trim_matches('"')
            .to_string(),
        warning,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn pending_refresh_unions_targets_without_binding_a_generation() {
        let first_node = Uuid::from_u128(1);
        let second_node = Uuid::from_u128(2);
        let edge = Uuid::from_u128(3);
        let pending = PendingGraphSummaryRefresh::targeted(vec![first_node], vec![edge])
            .merge(PendingGraphSummaryRefresh::targeted(vec![first_node, second_node], Vec::new()));

        assert!(pending.is_targeted());
        assert_eq!(pending.node_ids, vec![first_node, second_node]);
        assert_eq!(pending.edge_ids, vec![edge]);
        assert_eq!(pending.bind_source_truth_version(9).source_truth_version, 9);
    }

    #[test]
    fn broad_pending_refresh_dominates_targeted_work() {
        let pending = PendingGraphSummaryRefresh::targeted(vec![Uuid::now_v7()], Vec::new())
            .merge(PendingGraphSummaryRefresh::broad());

        assert!(pending.broad_refresh);
        assert!(!pending.is_targeted());
    }

    #[test]
    fn classify_summary_confidence_marks_conflicts() {
        let (status, warning) = classify_summary_confidence(3, &serde_json::json!({}), 2);

        assert_eq!(status, "conflicted");
        assert!(warning.is_some());
    }

    #[test]
    fn evidence_fragment_selection_retains_smallest_normalized_unique_values() {
        let texts = ["Zebra", " Beta\n value ", "", "Alpha!", "Beta value.", "  "];
        for limit in [0, 1, 2, 8] {
            let expected = texts
                .iter()
                .filter_map(|text| normalize_fragment(text))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(limit)
                .collect::<Vec<_>>();
            let mut fragments = BTreeSet::new();
            for text in texts {
                retain_evidence_fragment(&mut fragments, text, limit);
                assert!(fragments.len() <= limit);
            }
            assert_eq!(fragments.into_iter().collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn evidence_fragment_selection_is_order_independent_after_unicode_truncation() {
        let long = "é".repeat(230);
        let texts = [long.as_str(), " beta", "alpha?", "beta.", "alpha?"];
        let mut forward = BTreeSet::new();
        let mut reverse = BTreeSet::new();
        for text in texts {
            retain_evidence_fragment(&mut forward, text, 2);
        }
        for text in texts.into_iter().rev() {
            retain_evidence_fragment(&mut reverse, text, 2);
        }
        assert_eq!(forward, reverse);
        assert_eq!(forward.into_iter().collect::<Vec<_>>(), vec!["alpha?", "beta."]);
        let mut single = BTreeSet::new();
        retain_evidence_fragment(&mut single, &long, 1);
        assert_eq!(single.first().unwrap().chars().count(), 220);
    }

    #[test]
    fn build_node_summary_includes_support_sentence() {
        let node = RuntimeGraphNodeRow {
            id: Uuid::nil(),
            library_id: Uuid::nil(),
            identity_key: "entity:budget".to_string(),
            label: "Budget".to_string(),
            node_type: "entity".to_string(),
            aliases_json: serde_json::json!(["Budget"]),
            summary: Some("Budget approval moved to Q2".to_string()),
            metadata_json: serde_json::json!({}),
            support_count: 3,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let fragments = load_summary_fragments(&node.metadata_json, node.summary.as_deref());
        let (summary, status, warning) = build_node_summary(&node, &fragments, &[]);

        assert!(summary.contains("Budget approval moved to Q2."));
        assert!(summary.contains("Supported by 3 active evidence lines."));
        assert_eq!(status, "strong");
        assert!(warning.is_none());
    }

    #[test]
    fn build_edge_summary_falls_back_to_relationship_sentence() {
        let edge = RuntimeGraphEdgeRow {
            id: Uuid::nil(),
            library_id: Uuid::nil(),
            from_node_id: Uuid::nil(),
            to_node_id: Uuid::now_v7(),
            relation_type: "reports_to".to_string(),
            identity_key: "entity:a--reports_to--entity:b".to_string(),
            summary: None,
            weight: None,
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let fragments = load_summary_fragments(&edge.metadata_json, edge.summary.as_deref());
        let (summary, status, warning) = build_edge_summary(&edge, "Alex", "Sam", &fragments, &[]);

        assert!(summary.contains("Alex reports to Sam."));
        assert_eq!(status, "weak");
        assert!(warning.is_some());
    }
}
