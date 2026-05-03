use std::collections::{BTreeSet, HashMap};

use tracing::warn;
use uuid::Uuid;

use crate::{
    domains::agent_runtime::{
        RuntimeExecutionSummary, RuntimePolicyDecision, RuntimePolicySummary,
    },
    domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryGraphEdgeReference,
        QueryGraphNodeReference, QueryRuntimeStageSummary, QueryVerificationState,
        TechnicalFactReference,
    },
    infra::{
        arangodb::{
            context_store::KnowledgeContextBundleReferenceSetRow,
            document_store::{KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow},
        },
        repositories::{self as graph_repo, query_repository, runtime_repository},
    },
    services::{
        graph::identity::normalize_graph_identity_component,
        query::execution::{
            explicit_document_reference_literals, normalized_document_target_candidates,
        },
    },
    shared::extraction::table_summary::is_table_summary_text,
};

use super::{
    MAX_ANSWER_SOURCE_LINKS, MAX_DETAIL_PREPARED_SEGMENT_REFERENCES,
    MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION, PREPARED_SEGMENT_FOCUS_MIN_TOKEN_LEN,
    PreparedSegmentRevisionInfo, RankedBundleReference, turn::query_runtime_stage_label,
};

fn execution_id_of(bundle: &KnowledgeContextBundleReferenceSetRow) -> Uuid {
    bundle
        .bundle
        .query_execution_id
        .expect("invariant: context bundle always carries query_execution_id")
}

pub(crate) fn build_prepared_segment_references(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    blocks: &[KnowledgeStructuredBlockRow],
    block_rank_refs: &HashMap<Uuid, RankedBundleReference>,
    query_text: &str,
    revision_info: &HashMap<Uuid, PreparedSegmentRevisionInfo>,
) -> Vec<PreparedSegmentReference> {
    let Some(bundle) = bundle else {
        return Vec::new();
    };
    let execution_id = execution_id_of(bundle);
    let query_focus_tokens = prepared_segment_focus_tokens(query_text);
    let explicit_document_literals = explicit_document_reference_literals(query_text);
    let explicit_document_literal = match explicit_document_literals.as_slice() {
        [literal] => Some(literal.as_str()),
        _ => None,
    };
    let _ = query_text;
    let table_aggregation = false;
    let latest_revision_by_document = latest_block_revision_by_document(blocks);
    let latest_revision_has_table_analytics =
        latest_revision_has_table_analytics(blocks, &latest_revision_by_document);
    let mut revision_focus_scores = HashMap::<Uuid, usize>::new();
    for block in blocks {
        if !block_rank_refs.contains_key(&block.block_id) {
            continue;
        }
        if table_aggregation
            && latest_revision_by_document.get(&block.document_id).copied()
                != Some(block.revision_id)
        {
            continue;
        }
        if table_aggregation
            && latest_revision_has_table_analytics.contains(&block.document_id)
            && !is_table_analytics_block(block)
        {
            continue;
        }
        let focus_score = prepared_segment_focus_score(&query_focus_tokens, block);
        if focus_score == 0 {
            continue;
        }
        revision_focus_scores
            .entry(block.revision_id)
            .and_modify(|current| *current = (*current).max(focus_score))
            .or_insert(focus_score);
    }
    let max_revision_focus_score = revision_focus_scores.values().copied().max().unwrap_or(0);
    let mut items = blocks
        .iter()
        .filter_map(|block| {
            let reference = block_rank_refs.get(&block.block_id)?;
            if max_revision_focus_score >= 2
                && revision_focus_scores.get(&block.revision_id).copied().unwrap_or(0)
                    < max_revision_focus_score
            {
                return None;
            }
            let block_kind = block.block_kind.parse().ok()?;
            let info = revision_info.get(&block.revision_id).cloned().unwrap_or_default();
            if explicit_document_literal.is_some_and(|literal| {
                !prepared_segment_matches_explicit_document_literal(literal, &info)
            }) {
                return None;
            }
            let reference = PreparedSegmentReference {
                execution_id,
                segment_id: block.block_id,
                revision_id: block.revision_id,
                block_kind,
                rank: reference.rank,
                score: reference.score,
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
                document_id: Some(block.document_id),
                document_title: info.document_title,
                source_uri: info.source_uri,
                source_access: info.source_access,
            };
            Some((
                reference,
                prepared_segment_focus_score(&query_focus_tokens, block),
                prepared_segment_kind_priority(block, table_aggregation),
                block.ordinal,
            ))
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.rank.cmp(&right.0.rank))
            .then_with(|| right.0.score.total_cmp(&left.0.score))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.segment_id.cmp(&right.0.segment_id))
    });
    let mut per_revision_counts = HashMap::<Uuid, usize>::new();
    let mut limited = Vec::with_capacity(items.len().min(MAX_DETAIL_PREPARED_SEGMENT_REFERENCES));
    for (reference, _, _, _) in items {
        let per_revision = per_revision_counts.entry(reference.revision_id).or_insert(0);
        if *per_revision >= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION {
            continue;
        }
        limited.push(reference);
        *per_revision += 1;
        if limited.len() >= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES {
            break;
        }
    }
    limited
}

pub(crate) fn build_assistant_document_references(
    execution_id: Uuid,
    references: &[crate::services::query::assistant_grounding::AssistantGroundingDocumentReference],
) -> Vec<PreparedSegmentReference> {
    references
        .iter()
        .map(|reference| {
            let revision_id = reference.revision_id.unwrap_or_else(|| {
                Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("assistant-grounding-revision:{}", reference.document_id).as_bytes(),
                )
            });
            let segment_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "assistant-grounding-segment:{}:{}:{}:{}",
                    reference.document_id,
                    revision_id,
                    reference.slice_start_offset,
                    reference.slice_end_offset
                )
                .as_bytes(),
            );
            PreparedSegmentReference {
                execution_id,
                segment_id,
                revision_id,
                block_kind:
                    crate::shared::extraction::structured_document::StructuredBlockKind::Paragraph,
                rank: reference.rank,
                score: 1.0,
                heading_trail: Vec::new(),
                section_path: (!reference.excerpt.is_empty())
                    .then(|| vec![reference.excerpt.clone()])
                    .unwrap_or_default(),
                document_id: Some(reference.document_id),
                document_title: Some(reference.document_title.clone()),
                source_uri: reference.source_uri.clone(),
                source_access: reference.source_access.clone(),
            }
        })
        .collect()
}

pub(crate) fn append_answer_source_links(
    mut answer: String,
    references: &[PreparedSegmentReference],
) -> String {
    let source_section = render_answer_source_links(references);
    if let Some(source_section) = source_section {
        if !answer.trim().is_empty() {
            answer.push_str("\n\n---\n");
        }
        answer.push_str(&source_section);
    }
    answer
}

pub(crate) fn render_answer_source_links(
    references: &[PreparedSegmentReference],
) -> Option<String> {
    let mut seen = BTreeSet::<String>::new();
    let mut lines = Vec::new();

    for reference in references {
        let access = match reference.source_access.as_ref() {
            Some(access) => access,
            None => continue,
        };
        if !seen.insert(access.href.clone()) {
            continue;
        }
        let label = answer_source_link_label(reference, access.href.as_str());
        lines.push(format!("- [{label}](<{}>)", access.href));
        if lines.len() >= MAX_ANSWER_SOURCE_LINKS {
            break;
        }
    }

    (!lines.is_empty()).then(|| format!("Sources\n{}", lines.join("\n")))
}

fn answer_source_link_label(reference: &PreparedSegmentReference, fallback_href: &str) -> String {
    reference
        .document_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            reference.heading_trail.last().map(String::as_str).and_then(|value| {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then_some(trimmed)
            })
        })
        .unwrap_or(fallback_href)
        .replace(['[', ']'], "")
        .replace('\n', " ")
}

pub(crate) fn prepared_segment_focus_tokens(query_text: &str) -> BTreeSet<String> {
    normalize_graph_identity_component(query_text)
        .split('_')
        .filter(|token| token.chars().count() >= PREPARED_SEGMENT_FOCUS_MIN_TOKEN_LEN)
        .map(str::to_string)
        .collect()
}

pub(crate) fn prepared_segment_focus_score(
    query_focus_tokens: &BTreeSet<String>,
    block: &KnowledgeStructuredBlockRow,
) -> usize {
    if query_focus_tokens.is_empty() {
        return 0;
    }
    let mut focus_haystack = String::new();
    if !block.heading_trail.is_empty() {
        focus_haystack.push_str(&block.heading_trail.join(" "));
        focus_haystack.push(' ');
    }
    if !block.section_path.is_empty() {
        focus_haystack.push_str(&block.section_path.join(" "));
    }
    let normalized_focus_haystack = normalize_graph_identity_component(&focus_haystack);
    let block_tokens = normalized_focus_haystack
        .split('_')
        .filter(|token| !token.is_empty())
        .collect::<BTreeSet<_>>();
    query_focus_tokens.iter().filter(|token| block_tokens.contains(token.as_str())).count()
}

fn prepared_segment_kind_priority(
    block: &KnowledgeStructuredBlockRow,
    table_aggregation: bool,
) -> u8 {
    if table_aggregation {
        if block.block_kind == "metadata_block" && is_table_summary_text(&block.normalized_text) {
            return 4;
        }
        return match block.block_kind.as_str() {
            "table_row" => 3,
            "table" => 2,
            "heading" | "endpoint_block" => 1,
            _ => 0,
        };
    }
    match block.block_kind.as_str() {
        "heading" | "endpoint_block" => 4,
        "paragraph" | "code_block" | "table_row" => 3,
        "list_item" | "table" => 2,
        "quote_block" | "metadata_block" | "source_profile" | "source_unit" => 1,
        _ => 0,
    }
}

fn latest_block_revision_by_document(
    blocks: &[KnowledgeStructuredBlockRow],
) -> HashMap<Uuid, Uuid> {
    let mut revisions = HashMap::<Uuid, Uuid>::new();
    for block in blocks {
        revisions
            .entry(block.document_id)
            .and_modify(|current| {
                if block.revision_id > *current {
                    *current = block.revision_id;
                }
            })
            .or_insert(block.revision_id);
    }
    revisions
}

fn latest_revision_has_table_analytics(
    blocks: &[KnowledgeStructuredBlockRow],
    latest_revision_by_document: &HashMap<Uuid, Uuid>,
) -> BTreeSet<Uuid> {
    blocks
        .iter()
        .filter(|block| {
            latest_revision_by_document.get(&block.document_id).copied() == Some(block.revision_id)
        })
        .filter(|block| is_table_analytics_block(block))
        .map(|block| block.document_id)
        .collect()
}

fn is_table_analytics_block(block: &KnowledgeStructuredBlockRow) -> bool {
    block.block_kind == "table_row"
        || (block.block_kind == "metadata_block" && is_table_summary_text(&block.normalized_text))
}

fn prepared_segment_matches_explicit_document_literal(
    literal: &str,
    info: &PreparedSegmentRevisionInfo,
) -> bool {
    info.document_title
        .as_deref()
        .map(|title| normalized_document_target_candidates([title]))
        .is_some_and(|candidates| candidates.iter().any(|candidate| candidate == literal))
}

pub(crate) fn build_technical_fact_references(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    facts: &[KnowledgeTechnicalFactRow],
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
) -> Vec<TechnicalFactReference> {
    let Some(bundle) = bundle else {
        return Vec::new();
    };
    let execution_id = execution_id_of(bundle);
    let mut items = facts
        .iter()
        .filter_map(|fact| {
            let reference = fact_rank_refs.get(&fact.fact_id)?;
            Some(TechnicalFactReference {
                execution_id,
                fact_id: fact.fact_id,
                revision_id: fact.revision_id,
                fact_kind: fact.fact_kind.parse().ok()?,
                canonical_value: fact.canonical_value_text.clone(),
                display_value: fact.display_value.clone(),
                rank: reference.rank,
                score: reference.score,
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    items
}

pub(crate) fn parse_query_verification_state(value: &str) -> QueryVerificationState {
    match value.trim().to_ascii_lowercase().as_str() {
        "verified" => QueryVerificationState::Verified,
        "partially_supported" => QueryVerificationState::PartiallySupported,
        "conflicting_evidence" | "conflicting" => QueryVerificationState::Conflicting,
        "insufficient_evidence" => QueryVerificationState::InsufficientEvidence,
        "failed" => QueryVerificationState::Failed,
        _ => QueryVerificationState::NotRun,
    }
}

pub(crate) fn parse_query_verification_warnings(
    value: &serde_json::Value,
) -> Vec<crate::domains::query::QueryVerificationWarning> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

pub(crate) fn map_chunk_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryChunkReference> {
    let execution_id = execution_id_of(bundle);
    bundle
        .chunk_references
        .iter()
        .map(|reference| QueryChunkReference {
            execution_id,
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

pub(crate) fn map_entity_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphNodeReference> {
    let execution_id = execution_id_of(bundle);
    bundle
        .entity_references
        .iter()
        .map(|reference| QueryGraphNodeReference {
            execution_id,
            node_id: reference.entity_id,
            rank: reference.rank,
            score: reference.score,
            label: String::new(),
            entity_type: None,
            summary: None,
        })
        .collect()
}

/// Searches PostgreSQL `runtime_graph_node` by keyword overlap with the query
/// text and returns ranked `QueryGraphNodeReference` entries.  This provides
/// entity references when the ArangoDB `knowledge_entity` collection is empty.
pub(crate) async fn search_pg_entity_references(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    execution_id: Uuid,
    query_text: &str,
) -> Vec<QueryGraphNodeReference> {
    const PG_ENTITY_SEARCH_LIMIT: i64 = 15;
    match graph_repo::search_runtime_graph_nodes_by_query_text(
        pool,
        library_id,
        query_text,
        PG_ENTITY_SEARCH_LIMIT,
    )
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| QueryGraphNodeReference {
                execution_id,
                node_id: row.id,
                rank: i32::try_from(index).unwrap_or(i32::MAX) + 1,
                score: f64::from(row.support_count),
                label: row.label,
                entity_type: Some(row.node_type),
                summary: row.summary,
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            warn!(
                error = %error,
                library_id = %library_id,
                execution_id = %execution_id,
                "PostgreSQL entity reference fallback search failed"
            );
            Vec::new()
        }
    }
}

pub(crate) fn map_relation_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphEdgeReference> {
    let execution_id = execution_id_of(bundle);
    bundle
        .relation_references
        .iter()
        .map(|reference| QueryGraphEdgeReference {
            execution_id,
            edge_id: reference.relation_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

pub(crate) fn map_execution_runtime_summary(
    row: &query_repository::QueryExecutionRow,
    runtime_policy_rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimeExecutionSummary {
    RuntimeExecutionSummary {
        runtime_execution_id: row.runtime_execution_id,
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        failure_code: row.failure_code.clone(),
        failure_summary_redacted: row
            .failure_summary_redacted
            .clone()
            .or_else(|| row.failure_code.clone()),
        policy_summary: map_runtime_policy_summary(runtime_policy_rows),
        accepted_at: row.started_at,
        completed_at: row.completed_at,
    }
}

fn map_runtime_policy_summary(
    rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimePolicySummary {
    crate::agent_runtime::trace::build_policy_summary(
        &rows
            .iter()
            .map(|row| RuntimePolicyDecision {
                id: row.id,
                runtime_execution_id: row.runtime_execution_id,
                stage_record_id: row.stage_record_id,
                action_record_id: row.action_record_id,
                target_kind: row.target_kind,
                decision_kind: row.decision_kind,
                reason_code: row.reason_code.clone(),
                reason_summary_redacted: row.reason_summary_redacted.clone(),
                created_at: row.created_at,
            })
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn map_execution_runtime_stage_summaries(
    row: &query_repository::QueryExecutionRow,
    runtime_stage_records: &[runtime_repository::RuntimeStageRecordRow],
) -> Vec<QueryRuntimeStageSummary> {
    if !runtime_stage_records.is_empty() {
        let mut seen = BTreeSet::new();
        return runtime_stage_records
            .iter()
            .map(|record| record.stage_kind)
            .filter(|stage_kind| seen.insert(*stage_kind))
            .map(|stage_kind| QueryRuntimeStageSummary {
                stage_kind,
                stage_label: query_runtime_stage_label(stage_kind).to_string(),
            })
            .collect();
    }

    row.runtime_active_stage
        .map(|stage_kind| {
            vec![QueryRuntimeStageSummary {
                stage_kind,
                stage_label: query_runtime_stage_label(stage_kind).to_string(),
            }]
        })
        .unwrap_or_default()
}
