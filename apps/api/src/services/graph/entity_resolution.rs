use std::collections::BTreeMap;

use anyhow::Result;
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphNodeRow},
};

/// Outcome of an entity resolution pass over a library's graph nodes.
#[derive(Debug, Default)]
pub struct EntityResolutionOutcome {
    pub merges_applied: usize,
    pub entities_removed: usize,
    pub aliases_added: usize,
}

/// Why two entities were merged.
#[derive(Debug)]
enum MergeReason {
    ExactAlias,
    NormalizedPrefix,
    Acronym,
}

/// A proposed merge of one entity into another.
#[derive(Debug)]
struct MergeCandidate {
    keep_node_id: Uuid,
    keep_key: String,
    remove_node_id: Uuid,
    remove_key: String,
    reason: MergeReason,
}

/// Deterministic entity resolution service that merges duplicate graph nodes
/// sharing the same real-world referent (e.g. "PostgreSQL" / "Postgres",
/// "JWT" / "JSON Web Token").
pub struct EntityResolutionService;

/// Common technology suffixes that can be stripped for normalized prefix matching.
const STRIPPABLE_SUFFIXES: &[&str] = &[
    "_database",
    "_db",
    "_protocol",
    "_framework",
    "_service",
    "_api",
    "_library",
    "_lang",
    "_language",
    "_server",
    "_client",
    "_tool",
    "_platform",
    "_system",
    "_engine",
    "_runtime",
];

/// Well-known abbreviation pairs: (short_form, long_form_normalized).
/// The long form is expressed as its `normalize_graph_identity_component` output.
const KNOWN_ABBREVIATIONS: &[(&str, &str)] = &[
    ("pg", "postgresql"),
    ("postgres", "postgresql"),
    ("k8s", "kubernetes"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("py", "python"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("tf", "terraform"),
    ("gql", "graphql"),
    ("mongo", "mongodb"),
    ("redis", "redis"),
    ("es", "elasticsearch"),
    ("aws", "amazon_web_services"),
    ("gcp", "google_cloud_platform"),
];

impl EntityResolutionService {
    /// Scans entities in a library for merge candidates based on normalized label
    /// similarity. This is a deterministic pass (no LLM) that catches obvious
    /// duplicates.
    pub async fn resolve_library_entities(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<EntityResolutionOutcome> {
        let pool = &state.persistence.postgres;

        let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id).await?;
        let projection_version = match snapshot {
            Some(s) => s.projection_version,
            None => return Ok(EntityResolutionOutcome::default()),
        };

        let nodes =
            repositories::list_runtime_graph_nodes_by_library(pool, library_id, projection_version)
                .await?;

        if nodes.len() < 2 {
            return Ok(EntityResolutionOutcome::default());
        }

        let candidates = find_merge_candidates(&nodes);
        if candidates.is_empty() {
            return Ok(EntityResolutionOutcome::default());
        }

        let mut outcome = EntityResolutionOutcome::default();
        for candidate in &candidates {
            let applied = execute_merge(pool, library_id, projection_version, candidate).await?;
            if applied {
                outcome.merges_applied += 1;
                outcome.entities_removed += 1;
                outcome.aliases_added += 1;
            }
        }

        info!(
            library_id = %library_id,
            merges = outcome.merges_applied,
            removed = outcome.entities_removed,
            aliases = outcome.aliases_added,
            "entity resolution pass complete"
        );

        Ok(outcome)
    }
}

/// Run entity resolution only when the library is large enough for it to matter.
pub async fn resolve_after_ingestion(
    state: &AppState,
    library_id: Uuid,
) -> Result<EntityResolutionOutcome> {
    let pool = &state.persistence.postgres;
    let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id).await?;
    let node_count = snapshot.as_ref().map_or(0, |s| s.node_count);
    if node_count < 50 {
        return Ok(EntityResolutionOutcome::default());
    }
    EntityResolutionService.resolve_library_entities(state, library_id).await
}

// ---------------------------------------------------------------------------
// Merge candidate detection
// ---------------------------------------------------------------------------

fn find_merge_candidates(nodes: &[RuntimeGraphNodeRow]) -> Vec<MergeCandidate> {
    let mut candidates = Vec::new();
    let mut already_removed: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    // Index: normalized_key (without type prefix) → list of nodes.
    let mut key_index: BTreeMap<String, Vec<&RuntimeGraphNodeRow>> = BTreeMap::new();
    for node in nodes {
        if node.node_type == "document" {
            continue;
        }
        let bare_key = strip_node_type_prefix(&node.canonical_key);
        key_index.entry(bare_key).or_default().push(node);
    }

    // Precompute normalized forms once per node. Pass 1 below is O(N²);
    // without caching, `check_alias_match` re-ran
    // `normalize_graph_identity_component` on both labels AND every
    // alias in both sides for every pair — at ~10k nodes and ~5 aliases
    // each that is ~250 M NFKC/lowercase/fold passes per resolution
    // round, which dominated the worker's CPU (a single resolution
    // pass burned multiple cores for tens of seconds). After the
    // precompute, each label and each alias is normalized exactly
    // once; Pass 1 does the pair scan with string comparisons only.
    let normalized: Vec<(String, Vec<String>)> = nodes
        .iter()
        .map(|node| {
            let label_norm =
                crate::services::graph::identity::normalize_graph_identity_component(&node.label);
            let alias_norms = node_aliases(node)
                .iter()
                .map(|alias| {
                    crate::services::graph::identity::normalize_graph_identity_component(alias)
                })
                .collect::<Vec<_>>();
            (label_norm, alias_norms)
        })
        .collect();

    // Pass 1: exact alias match — if entity A's label appears in entity B's aliases.
    for (i, a) in nodes.iter().enumerate() {
        if a.node_type == "document" || already_removed.contains(&a.id) {
            continue;
        }
        let (a_label_norm, a_alias_norms) = &normalized[i];
        for (offset, b) in nodes[i + 1..].iter().enumerate() {
            if b.node_type == "document" || already_removed.contains(&b.id) {
                continue;
            }
            let j = i + 1 + offset;
            let (b_label_norm, b_alias_norms) = &normalized[j];
            if let Some(candidate) = check_alias_match_precomputed(
                a,
                b,
                a_label_norm,
                a_alias_norms,
                b_label_norm,
                b_alias_norms,
            ) {
                already_removed.insert(candidate.remove_node_id);
                candidates.push(candidate);
            }
        }
    }

    // Pass 2: normalized prefix match — strip common suffixes.
    let mut stripped_index: BTreeMap<String, Vec<&RuntimeGraphNodeRow>> = BTreeMap::new();
    for node in nodes {
        if node.node_type == "document" || already_removed.contains(&node.id) {
            continue;
        }
        let bare = strip_node_type_prefix(&node.canonical_key);
        let stripped = strip_known_suffixes(&bare);
        stripped_index.entry(stripped).or_default().push(node);
    }
    for group in stripped_index.values() {
        if group.len() < 2 {
            continue;
        }
        let (keep, rest) = pick_canonical_from_group(group);
        for remove in rest {
            if already_removed.contains(&remove.id) {
                continue;
            }
            already_removed.insert(remove.id);
            candidates.push(MergeCandidate {
                keep_node_id: keep.id,
                keep_key: keep.canonical_key.clone(),
                remove_node_id: remove.id,
                remove_key: remove.canonical_key.clone(),
                reason: MergeReason::NormalizedPrefix,
            });
        }
    }

    // Pass 3: acronym / known abbreviation match.
    for (i, a) in nodes.iter().enumerate() {
        if a.node_type == "document" || already_removed.contains(&a.id) {
            continue;
        }
        for b in &nodes[i + 1..] {
            if b.node_type == "document" || already_removed.contains(&b.id) {
                continue;
            }
            if let Some(candidate) = check_acronym_match(a, b) {
                already_removed.insert(candidate.remove_node_id);
                candidates.push(candidate);
            }
        }
    }

    candidates
}

fn strip_node_type_prefix(canonical_key: &str) -> String {
    canonical_key
        .split_once(':')
        .map_or_else(|| canonical_key.to_string(), |(_, rest)| rest.to_string())
}

fn strip_known_suffixes(key: &str) -> String {
    let mut result = key.to_string();
    for suffix in STRIPPABLE_SUFFIXES {
        if let Some(stripped) = result.strip_suffix(suffix) {
            if !stripped.is_empty() {
                result = stripped.to_string();
                break;
            }
        }
    }
    result
}

fn node_aliases(node: &RuntimeGraphNodeRow) -> Vec<String> {
    node.aliases_json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|s| s.to_string())
        .collect()
}

fn check_alias_match(a: &RuntimeGraphNodeRow, b: &RuntimeGraphNodeRow) -> Option<MergeCandidate> {
    let a_aliases = node_aliases(a);
    let b_aliases = node_aliases(b);
    let a_norm = crate::services::graph::identity::normalize_graph_identity_component(&a.label);
    let b_norm = crate::services::graph::identity::normalize_graph_identity_component(&b.label);
    let a_alias_norms = a_aliases
        .iter()
        .map(|alias| crate::services::graph::identity::normalize_graph_identity_component(alias))
        .collect::<Vec<_>>();
    let b_alias_norms = b_aliases
        .iter()
        .map(|alias| crate::services::graph::identity::normalize_graph_identity_component(alias))
        .collect::<Vec<_>>();
    check_alias_match_precomputed(a, b, &a_norm, &a_alias_norms, &b_norm, &b_alias_norms)
}

/// Same contract as [`check_alias_match`] but with the normalized label
/// and normalized-aliases vectors provided by the caller. Used by the
/// O(N²) pass in [`find_merge_candidates`] so the expensive NFKC pass
/// only runs once per node instead of once per pair.
fn check_alias_match_precomputed(
    a: &RuntimeGraphNodeRow,
    b: &RuntimeGraphNodeRow,
    a_label_norm: &str,
    a_alias_norms: &[String],
    b_label_norm: &str,
    b_alias_norms: &[String],
) -> Option<MergeCandidate> {
    let b_in_a = a_alias_norms.iter().any(|alias| alias == b_label_norm);
    let a_in_b = b_alias_norms.iter().any(|alias| alias == a_label_norm);

    if !b_in_a && !a_in_b {
        return None;
    }

    let (keep, remove) = pick_canonical_pair(a, b);
    Some(MergeCandidate {
        keep_node_id: keep.id,
        keep_key: keep.canonical_key.clone(),
        remove_node_id: remove.id,
        remove_key: remove.canonical_key.clone(),
        reason: MergeReason::ExactAlias,
    })
}

fn check_acronym_match(a: &RuntimeGraphNodeRow, b: &RuntimeGraphNodeRow) -> Option<MergeCandidate> {
    let a_norm = strip_node_type_prefix(&a.canonical_key);
    let b_norm = strip_node_type_prefix(&b.canonical_key);

    // Check known abbreviation table.
    let matched = KNOWN_ABBREVIATIONS.iter().any(|(short, long)| {
        (a_norm == *short && b_norm == *long) || (a_norm == *long && b_norm == *short)
    });

    if matched {
        let (keep, remove) = pick_canonical_pair(a, b);
        return Some(MergeCandidate {
            keep_node_id: keep.id,
            keep_key: keep.canonical_key.clone(),
            remove_node_id: remove.id,
            remove_key: remove.canonical_key.clone(),
            reason: MergeReason::Acronym,
        });
    }

    // Check if one label is an acronym of the other.
    if is_acronym_of(&a.label, &b.label) || is_acronym_of(&b.label, &a.label) {
        let (keep, remove) = pick_canonical_pair(a, b);
        return Some(MergeCandidate {
            keep_node_id: keep.id,
            keep_key: keep.canonical_key.clone(),
            remove_node_id: remove.id,
            remove_key: remove.canonical_key.clone(),
            reason: MergeReason::Acronym,
        });
    }

    None
}

/// Returns true if `short` is an acronym formed from the first letters of the
/// words in `long` (case-insensitive).
fn is_acronym_of(short: &str, long: &str) -> bool {
    let short_upper = short.trim().to_uppercase();
    if short_upper.len() < 2 {
        return false;
    }
    // All characters in short must be alphabetic for a valid acronym.
    if !short_upper.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let words: Vec<&str> = long.split_whitespace().collect();
    if words.len() < 2 || words.len() != short_upper.len() {
        return false;
    }
    words.iter().zip(short_upper.chars()).all(|(word, ch)| word.to_uppercase().starts_with(ch))
}

/// Pick which entity to keep vs remove. Prefer higher support_count; break ties
/// by shorter canonical_key (more general), then by earlier creation time.
fn pick_canonical_pair<'a>(
    a: &'a RuntimeGraphNodeRow,
    b: &'a RuntimeGraphNodeRow,
) -> (&'a RuntimeGraphNodeRow, &'a RuntimeGraphNodeRow) {
    if a.support_count > b.support_count {
        (a, b)
    } else if b.support_count > a.support_count {
        (b, a)
    } else if a.canonical_key.len() <= b.canonical_key.len() {
        (a, b)
    } else {
        (b, a)
    }
}

fn pick_canonical_from_group<'a>(
    group: &[&'a RuntimeGraphNodeRow],
) -> (&'a RuntimeGraphNodeRow, Vec<&'a RuntimeGraphNodeRow>) {
    let mut sorted: Vec<&RuntimeGraphNodeRow> = group.to_vec();
    sorted.sort_by(|a, b| {
        b.support_count
            .cmp(&a.support_count)
            .then_with(|| a.canonical_key.len().cmp(&b.canonical_key.len()))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    let keep = sorted[0];
    let rest = sorted[1..].to_vec();
    (keep, rest)
}

// ---------------------------------------------------------------------------
// Merge execution
// ---------------------------------------------------------------------------

async fn execute_merge(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
    candidate: &MergeCandidate,
) -> Result<bool> {
    // Verify both nodes still exist (earlier merge in this batch may have removed one).
    let keep_node =
        repositories::get_runtime_graph_node_by_id(pool, library_id, candidate.keep_node_id)
            .await?;
    let remove_node =
        repositories::get_runtime_graph_node_by_id(pool, library_id, candidate.remove_node_id)
            .await?;

    let (keep_node, remove_node) = match (keep_node, remove_node) {
        (Some(k), Some(r)) => (k, r),
        _ => return Ok(false),
    };

    // 1. Re-point edges from the removed node to the kept node.
    sqlx::query(
        "update runtime_graph_edge
         set from_node_id = $1, updated_at = now()
         where library_id = $2 and from_node_id = $3 and projection_version = $4",
    )
    .bind(candidate.keep_node_id)
    .bind(library_id)
    .bind(candidate.remove_node_id)
    .bind(projection_version)
    .execute(pool)
    .await?;

    sqlx::query(
        "update runtime_graph_edge
         set to_node_id = $1, updated_at = now()
         where library_id = $2 and to_node_id = $3 and projection_version = $4",
    )
    .bind(candidate.keep_node_id)
    .bind(library_id)
    .bind(candidate.remove_node_id)
    .bind(projection_version)
    .execute(pool)
    .await?;

    // 2. Re-point evidence from the removed node to the kept node.
    sqlx::query(
        "update runtime_graph_evidence
         set target_id = $1
         where library_id = $2 and target_id = $3 and target_kind = 'node'",
    )
    .bind(candidate.keep_node_id)
    .bind(library_id)
    .bind(candidate.remove_node_id)
    .execute(pool)
    .await?;

    // 3. Merge aliases: combine both nodes' aliases and add the removed node's label.
    let mut merged_aliases: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for alias in node_aliases(&keep_node) {
        merged_aliases.insert(alias);
    }
    for alias in node_aliases(&remove_node) {
        merged_aliases.insert(alias);
    }
    merged_aliases.insert(keep_node.label.clone());
    merged_aliases.insert(remove_node.label.clone());
    let aliases_json = serde_json::to_value(merged_aliases.into_iter().collect::<Vec<_>>())
        .unwrap_or_else(|_| serde_json::json!([]));

    // 4. Sum support counts.
    let combined_support = keep_node.support_count + remove_node.support_count;

    // 5. Update the kept node.
    sqlx::query(
        "update runtime_graph_node
         set aliases_json = $1, support_count = $2, updated_at = now()
         where library_id = $3 and id = $4",
    )
    .bind(&aliases_json)
    .bind(combined_support)
    .bind(library_id)
    .bind(candidate.keep_node_id)
    .execute(pool)
    .await?;

    // 6. Delete the removed node.
    sqlx::query(
        "delete from runtime_graph_node
         where library_id = $1 and id = $2",
    )
    .bind(library_id)
    .bind(candidate.remove_node_id)
    .execute(pool)
    .await?;

    // 7. Collapse any parallel edges that step 1-2 just created on the kept
    //    node. Without this, two rows can share `(from_node_id,
    //    relation_type, to_node_id)` with different stale canonical_keys, and
    //    step 9 below would try to rename both to the same canonical_key,
    //    tripping the `runtime_graph_edge_..._canonical_key_..._key` unique
    //    constraint and silently aborting the rest of the merge.
    let collapsed_survivor_ids = collapse_parallel_edges_for_node(
        pool,
        library_id,
        projection_version,
        candidate.keep_node_id,
    )
    .await?;

    // 8. Re-derive `support_count` on every survivor that just absorbed a
    //    duplicate. `support_count` is denormalized from active evidence —
    //    after step 7 repoints evidence onto the survivor, the canonical
    //    counter must be recomputed instead of summed (a sum would
    //    double-count any evidence already attached to the survivor).
    if !collapsed_survivor_ids.is_empty() {
        repositories::recalculate_runtime_graph_edge_support_counts_by_ids(
            pool,
            library_id,
            projection_version,
            &collapsed_survivor_ids,
        )
        .await?;
    }

    // 9. Recalculate canonical_key for edges that now reference the kept node,
    //    to avoid stale keys containing the removed node's identity.
    recalculate_edge_canonical_keys(pool, library_id, projection_version, candidate.keep_node_id)
        .await?;

    info!(
        keep = %candidate.keep_key,
        remove = %candidate.remove_key,
        reason = ?candidate.reason,
        "merged duplicate entity"
    );

    Ok(true)
}

/// Collapses parallel edges that share `(from_node_id, relation_type,
/// to_node_id)` after a node merge re-pointed one side onto the kept node.
///
/// For each duplicate group, the oldest row (by `created_at`, then `id`)
/// survives. Every soft reference to a loser edge id is repointed at the
/// survivor (`runtime_graph_evidence`) or deleted (`runtime_graph_canonical_summary`,
/// `runtime_vector_target`), then the loser rows are dropped.
///
/// `support_count` is intentionally **not** summed onto the survivor here
/// — it is denormalized from active evidence and any drift would be wrong.
/// The caller follows up with the canonical
/// `recalculate_runtime_graph_edge_support_counts_by_ids` so the survivor
/// reflects the true count of active evidence after repointing.
///
/// All mutations across `runtime_graph_edge`,
/// `runtime_graph_evidence`, `runtime_graph_canonical_summary`, and
/// `runtime_vector_target` run inside one SQL statement using Postgres
/// data-modifying CTEs, which guarantees they execute exactly once against
/// a single snapshot of `runtime_graph_edge`.
///
/// Returns the surviving edge ids that absorbed at least one duplicate, so
/// the caller can target the support-count recompute precisely.
async fn collapse_parallel_edges_for_node(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
    kept_node_id: Uuid,
) -> Result<Vec<Uuid>> {
    let survivor_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        with grouped as (
            select id,
                   from_node_id,
                   relation_type,
                   to_node_id,
                   row_number() over w as rn,
                   first_value(id) over w as survivor_id
              from runtime_graph_edge
             where library_id = $1
               and projection_version = $2
               and (from_node_id = $3 or to_node_id = $3)
            window w as (
                partition by from_node_id, relation_type, to_node_id
                order by created_at asc, id asc
            )
        ),
        losers as (
            select id as loser_id, survivor_id
              from grouped
             where rn > 1
        ),
        repoint_evidence as (
            update runtime_graph_evidence ev
               set target_id = losers.survivor_id
              from losers
             where ev.target_kind = 'edge'
               and ev.library_id = $1
               and ev.target_id = losers.loser_id
            returning 1
        ),
        drop_summaries as (
            delete from runtime_graph_canonical_summary s
             using losers
             where s.library_id = $1
               and s.target_kind = 'edge'
               and s.target_id = losers.loser_id
            returning 1
        ),
        drop_vectors as (
            delete from runtime_vector_target v
             using losers
             where v.library_id = $1
               and v.target_kind = 'edge'
               and v.target_id = losers.loser_id
            returning 1
        ),
        delete_losers as (
            delete from runtime_graph_edge
             using losers
             where runtime_graph_edge.id = losers.loser_id
            returning 1
        )
        select distinct survivor_id
          from grouped
         where rn = 1
           and exists (
               select 1 from grouped g2
                where g2.from_node_id = grouped.from_node_id
                  and g2.relation_type = grouped.relation_type
                  and g2.to_node_id = grouped.to_node_id
                  and g2.rn > 1
           )
        "#,
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(kept_node_id)
    .fetch_all(pool)
    .await?;
    Ok(survivor_ids)
}

/// Recalculates canonical_key for every edge touching a given node, using the
/// current canonical_key values of the endpoint nodes.
async fn recalculate_edge_canonical_keys(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_id: Uuid,
) -> Result<()> {
    let edges = repositories::list_admitted_runtime_graph_edges_by_node_ids(
        pool,
        library_id,
        projection_version,
        &[node_id],
    )
    .await?;

    for edge in &edges {
        let from_node =
            repositories::get_runtime_graph_node_by_id(pool, library_id, edge.from_node_id).await?;
        let to_node =
            repositories::get_runtime_graph_node_by_id(pool, library_id, edge.to_node_id).await?;
        if let (Some(from), Some(to)) = (from_node, to_node) {
            let new_key = crate::services::graph::identity::canonical_edge_key(
                &from.canonical_key,
                &edge.relation_type,
                &to.canonical_key,
            );
            if new_key != edge.canonical_key {
                sqlx::query(
                    "update runtime_graph_edge
                     set canonical_key = $1, updated_at = now()
                     where library_id = $2 and id = $3",
                )
                .bind(&new_key)
                .bind(library_id)
                .bind(edge.id)
                .execute(pool)
                .await?;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_known_suffixes_removes_database_suffix() {
        assert_eq!(strip_known_suffixes("redis_database"), "redis");
        assert_eq!(strip_known_suffixes("redis_db"), "redis");
    }

    #[test]
    fn strip_known_suffixes_removes_framework_suffix() {
        assert_eq!(strip_known_suffixes("react_framework"), "react");
    }

    #[test]
    fn strip_known_suffixes_preserves_unmatched_keys() {
        assert_eq!(strip_known_suffixes("postgresql"), "postgresql");
        assert_eq!(strip_known_suffixes("redis"), "redis");
    }

    #[test]
    fn strip_known_suffixes_does_not_strip_to_empty() {
        // "_database" alone should not be stripped to empty.
        assert_eq!(strip_known_suffixes("_database"), "_database");
    }

    #[test]
    fn normalized_prefix_match_detects_redis_variants() {
        let bare_a = strip_known_suffixes("redis");
        let bare_b = strip_known_suffixes("redis_database");
        assert_eq!(bare_a, bare_b);
    }

    #[test]
    fn is_acronym_of_detects_jwt() {
        assert!(is_acronym_of("JWT", "JSON Web Token"));
    }

    #[test]
    fn is_acronym_of_detects_api() {
        assert!(is_acronym_of("API", "Application Programming Interface"));
    }

    #[test]
    fn is_acronym_of_rejects_short_input() {
        assert!(!is_acronym_of("A", "Apple"));
    }

    #[test]
    fn is_acronym_of_rejects_length_mismatch() {
        assert!(!is_acronym_of("JWT", "JSON Web"));
    }

    #[test]
    fn is_acronym_of_is_case_insensitive() {
        assert!(is_acronym_of("jwt", "JSON Web Token"));
    }

    #[test]
    fn pick_canonical_pair_prefers_higher_support_count() {
        let now = chrono::Utc::now();
        let a = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            canonical_key: "entity:postgresql".into(),
            label: "PostgreSQL".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 5,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };
        let b = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            canonical_key: "entity:postgres".into(),
            label: "Postgres".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 3,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };

        let (keep, remove) = pick_canonical_pair(&a, &b);
        assert_eq!(keep.id, a.id);
        assert_eq!(remove.id, b.id);
    }

    #[test]
    fn pick_canonical_pair_prefers_shorter_key_on_tie() {
        let now = chrono::Utc::now();
        let lib = Uuid::now_v7();
        let a = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:redis".into(),
            label: "Redis".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 2,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };
        let b = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:redis_database".into(),
            label: "Redis Database".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 2,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };

        let (keep, remove) = pick_canonical_pair(&a, &b);
        assert_eq!(keep.id, a.id);
        assert_eq!(remove.id, b.id);
    }

    #[test]
    fn known_abbreviation_table_contains_common_tech_pairs() {
        let has = |short: &str, long: &str| {
            KNOWN_ABBREVIATIONS.iter().any(|(s, l)| *s == short && *l == long)
        };
        assert!(has("pg", "postgresql"));
        assert!(has("postgres", "postgresql"));
        assert!(has("k8s", "kubernetes"));
        assert!(has("js", "javascript"));
    }

    #[test]
    fn check_alias_match_finds_exact_alias_overlap() {
        let now = chrono::Utc::now();
        let lib = Uuid::now_v7();
        let a = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:postgresql".into(),
            label: "PostgreSQL".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!(["PostgreSQL", "Postgres"]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 5,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };
        let b = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:postgres".into(),
            label: "Postgres".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!(["Postgres"]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 2,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };

        let candidate = check_alias_match(&a, &b);
        assert!(candidate.is_some());
        let candidate = candidate.unwrap();
        assert_eq!(candidate.keep_node_id, a.id);
        assert_eq!(candidate.remove_node_id, b.id);
    }

    #[test]
    fn check_acronym_match_finds_jwt() {
        let now = chrono::Utc::now();
        let lib = Uuid::now_v7();
        let a = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:jwt".into(),
            label: "JWT".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 3,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };
        let b = RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: lib,
            canonical_key: "entity:json_web_token".into(),
            label: "JSON Web Token".into(),
            node_type: "entity".into(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 5,
            projection_version: 1,
            created_at: now,
            updated_at: now,
        };

        let candidate = check_acronym_match(&a, &b);
        assert!(candidate.is_some());
        let candidate = candidate.unwrap();
        // "JSON Web Token" has higher support, so it should be kept.
        assert_eq!(candidate.keep_node_id, b.id);
        assert_eq!(candidate.remove_node_id, a.id);
    }

    #[test]
    fn find_merge_candidates_combines_all_passes() {
        let now = chrono::Utc::now();
        let lib = Uuid::now_v7();
        let nodes = vec![
            RuntimeGraphNodeRow {
                id: Uuid::now_v7(),
                library_id: lib,
                canonical_key: "entity:redis".into(),
                label: "Redis".into(),
                node_type: "entity".into(),
                aliases_json: serde_json::json!(["Redis"]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 5,
                projection_version: 1,
                created_at: now,
                updated_at: now,
            },
            RuntimeGraphNodeRow {
                id: Uuid::now_v7(),
                library_id: lib,
                canonical_key: "entity:redis_database".into(),
                label: "Redis Database".into(),
                node_type: "entity".into(),
                aliases_json: serde_json::json!(["Redis Database"]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 2,
                projection_version: 1,
                created_at: now,
                updated_at: now,
            },
        ];

        let candidates = find_merge_candidates(&nodes);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].keep_node_id, nodes[0].id);
        assert_eq!(candidates[0].remove_node_id, nodes[1].id);
    }

    #[test]
    fn document_nodes_are_excluded_from_resolution() {
        let now = chrono::Utc::now();
        let lib = Uuid::now_v7();
        let nodes = vec![
            RuntimeGraphNodeRow {
                id: Uuid::now_v7(),
                library_id: lib,
                canonical_key: "document:abc".into(),
                label: "doc abc".into(),
                node_type: "document".into(),
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 1,
                created_at: now,
                updated_at: now,
            },
            RuntimeGraphNodeRow {
                id: Uuid::now_v7(),
                library_id: lib,
                canonical_key: "document:abc_database".into(),
                label: "doc abc database".into(),
                node_type: "document".into(),
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 1,
                created_at: now,
                updated_at: now,
            },
        ];

        let candidates = find_merge_candidates(&nodes);
        assert!(candidates.is_empty());
    }
}
