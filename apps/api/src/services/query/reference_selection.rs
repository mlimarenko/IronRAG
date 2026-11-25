//! One bounded post-retrieval reference selection policy for every projection.
//!
//! Retrieval produces ranked references; user-facing projections show a small
//! slice. Ordering that slice by `(rank, kind)` alone lets one document's
//! technical metadata fill every slot and hide the sources carrying the
//! substantive claims. MCP compact output and UI evidence projection call this
//! one function instead of keeping parallel selection algorithms.

use std::collections::HashMap;

use uuid::Uuid;

/// Group key for one authorized document's chunk and segment references.
pub(crate) fn document_group_key(document_id: Uuid) -> String {
    format!("document:{document_id}")
}

/// Group key for a chunk without a hydrated document.
pub(crate) fn chunk_group_key(chunk_id: Uuid) -> String {
    format!("chunk:{chunk_id}")
}

/// Group key for a segment without a hydrated document.
pub(crate) fn segment_group_key(segment_id: Uuid) -> String {
    format!("segment:{segment_id}")
}

/// Group key for one revision's technical metadata.
pub(crate) fn revision_group_key(revision_id: Uuid) -> String {
    format!("revision:{revision_id}")
}

/// Group key for one graph entity.
pub(crate) fn entity_group_key(node_id: Uuid) -> String {
    format!("entity:{node_id}")
}

/// Group key for one graph relation.
pub(crate) fn relation_group_key(edge_id: Uuid) -> String {
    format!("relation:{edge_id}")
}

/// Maximum references one non-document group may contribute once claim-linked
/// and document-backed references have been selected.
pub(crate) const REFERENCE_METADATA_GROUP_LIMIT: usize = 2;

/// Projection-neutral ranking facts for one persisted reference.
#[derive(Debug, Clone)]
pub(crate) struct ReferenceSelectionCandidate {
    /// Rank assigned by retrieval.
    pub(crate) rank: i32,
    /// Stable kind precedence used to break rank ties.
    pub(crate) kind_order: u8,
    /// Identity whose references must not consume the whole slice alone.
    pub(crate) group: String,
    /// Whether the reference identifies a document chunk or segment.
    pub(crate) document_backed: bool,
}

/// Bounded selection plus how many claim units it attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReferenceSelection {
    /// Selected candidate indices in priority order.
    pub(crate) indices: Vec<usize>,
    /// Substantive units with at least one visible reference.
    pub(crate) attributed_unit_count: usize,
}

/// Selects at most `limit` references.
///
/// `claim_units` holds, per substantive claim unit in answer order, the
/// candidate indices linked by the server-owned semantic evidence. Consumers
/// resolve locators to candidates; an unresolved unit stays empty and is
/// counted as unattributed instead of being replaced by a fabricated source.
pub(crate) fn select_reference_indices(
    candidates: &[ReferenceSelectionCandidate],
    claim_units: &[Vec<usize>],
    limit: usize,
) -> ReferenceSelection {
    let mut selected = Vec::new();
    let mut used = vec![false; candidates.len()];
    let max_round = claim_units.iter().map(Vec::len).max().unwrap_or(0);
    let mut round = 0;
    while round < max_round && selected.len() < limit {
        for unit_candidates in claim_units {
            if selected.len() >= limit {
                break;
            }
            let Some(&index) = unit_candidates.get(round) else {
                continue;
            };
            let Some(is_used) = used.get(index) else {
                continue;
            };
            if *is_used {
                continue;
            }
            used[index] = true;
            selected.push(index);
        }
        // A round may add nothing because every candidate at that evidence
        // position was already selected by another unit. Later positions can
        // still hold unseen evidence, so continue to the evidence exhaustion.
        round += 1;
    }

    let mut document_order = (0..candidates.len())
        .filter(|&index| !used[index] && candidates[index].document_backed)
        .collect::<Vec<_>>();
    document_order
        .sort_by_key(|&index| (candidates[index].rank, candidates[index].kind_order, index));
    let document_start = selected.len();
    select_diverse(candidates, &document_order, limit, &mut selected, &mut used, None);
    selected[document_start..]
        .sort_by_key(|&index| (candidates[index].rank, candidates[index].kind_order, index));

    let mut metadata_order = (0..candidates.len())
        .filter(|&index| !used[index] && !candidates[index].document_backed)
        .collect::<Vec<_>>();
    metadata_order
        .sort_by_key(|&index| (candidates[index].rank, candidates[index].kind_order, index));
    let metadata_start = selected.len();
    select_diverse(
        candidates,
        &metadata_order,
        limit,
        &mut selected,
        &mut used,
        Some(REFERENCE_METADATA_GROUP_LIMIT),
    );
    selected[metadata_start..]
        .sort_by_key(|&index| (candidates[index].rank, candidates[index].kind_order, index));

    let attributed_unit_count = claim_units
        .iter()
        .filter(|unit| unit.iter().any(|&index| used.get(index).copied().unwrap_or(false)))
        .count();
    ReferenceSelection { indices: selected, attributed_unit_count }
}

fn select_diverse(
    candidates: &[ReferenceSelectionCandidate],
    order: &[usize],
    limit: usize,
    selected: &mut Vec<usize>,
    used: &mut [bool],
    group_limit: Option<usize>,
) {
    let mut group_counts: HashMap<&str, usize> = HashMap::new();
    let mut round = 0;
    loop {
        let before = selected.len();
        for &index in order {
            if selected.len() >= limit {
                return;
            }
            if used[index] {
                continue;
            }
            let count = group_counts.get(candidates[index].group.as_str()).copied().unwrap_or(0);
            let allowed = match group_limit {
                Some(group_limit) => count < group_limit,
                None => count <= round,
            };
            if !allowed {
                continue;
            }
            used[index] = true;
            *group_counts.entry(candidates[index].group.as_str()).or_insert(0) += 1;
            selected.push(index);
        }
        if selected.len() >= limit || selected.len() == before {
            break;
        }
        round += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{ReferenceSelectionCandidate, select_reference_indices};

    fn candidate(
        rank: i32,
        kind_order: u8,
        group: &str,
        document_backed: bool,
    ) -> ReferenceSelectionCandidate {
        ReferenceSelectionCandidate { rank, kind_order, group: group.to_string(), document_backed }
    }

    #[test]
    fn claim_units_get_one_source_before_repeated_evidence() {
        let candidates = vec![
            candidate(9, 0, "document:a", true),
            candidate(8, 0, "document:a", true),
            candidate(1, 2, "revision:r", false),
        ];
        let claim_units = vec![vec![0, 1]];
        let selection = select_reference_indices(&candidates, &claim_units, 2);
        assert_eq!(selection.indices, vec![0, 1]);
        assert_eq!(selection.attributed_unit_count, 1);
    }

    #[test]
    fn document_sources_precede_metadata_and_cap_one_group_after_selection() {
        let candidates = vec![
            candidate(1, 2, "revision:r", false),
            candidate(1, 2, "revision:r", false),
            candidate(1, 2, "revision:r", false),
            candidate(1, 2, "revision:r", false),
            candidate(2, 0, "document:a", true),
            candidate(20, 0, "document:b", true),
        ];
        let selection = select_reference_indices(&candidates, &[], 6);
        assert_eq!(selection.indices, vec![4, 5, 0, 1]);
    }

    #[test]
    fn lower_ranked_document_diverse_source_is_selected_beyond_sixteen() {
        let mut candidates =
            (1..=20).map(|rank| candidate(rank, 0, "document:a", true)).collect::<Vec<_>>();
        candidates.push(candidate(40, 0, "document:b", true));
        let selection = select_reference_indices(&candidates, &[], 2);
        assert_eq!(selection.indices, vec![0, 20]);
    }

    #[test]
    fn duplicate_evidence_positions_do_not_skip_later_candidates() {
        let candidates =
            vec![candidate(9, 0, "document:c1", true), candidate(9, 0, "document:c2", true)];
        let selection = select_reference_indices(&candidates, &[vec![0, 0, 1]], 2);
        assert_eq!(selection.indices, vec![0, 1]);
    }

    #[test]
    fn crossed_claim_prefix_keeps_later_unseen_evidence_prioritized() {
        let candidates = vec![
            candidate(9, 0, "document:c1", true),
            candidate(9, 0, "document:c2", true),
            candidate(9, 0, "document:c3", true),
            candidate(9, 0, "document:c4", true),
            candidate(1, 2, "revision:metadata", false),
            candidate(1, 0, "document:unrelated", true),
        ];
        let claim_units = vec![vec![0, 1, 2], vec![1, 0, 3]];
        let selection = select_reference_indices(&candidates, &claim_units, 4);
        assert_eq!(selection.indices, vec![0, 1, 2, 3]);
        assert_eq!(selection.attributed_unit_count, 2);
    }

    #[test]
    fn mixed_kind_claim_selection_keeps_every_chunk_in_the_chunk_only_set() {
        let full_candidates = vec![
            candidate(9, 0, "document:c1", true),
            candidate(9, 0, "document:c2", true),
            candidate(9, 0, "document:c3", true),
            candidate(9, 0, "document:c4", true),
            candidate(1, 1, "document:segment", true),
            candidate(1, 2, "revision:fact", false),
        ];
        let full_claim_units = vec![vec![0, 1, 2, 4], vec![1, 0, 3]];
        let full = select_reference_indices(&full_candidates, &full_claim_units, 6);
        let selected_chunks =
            full.indices.into_iter().filter(|&index| index < 4).collect::<Vec<_>>();

        let chunk_candidates = full_candidates[..4].to_vec();
        let chunk_claim_units = vec![vec![0, 1, 2], vec![1, 0, 3]];
        let chunk_only = select_reference_indices(&chunk_candidates, &chunk_claim_units, 6);

        assert!(selected_chunks.iter().all(|index| chunk_only.indices.contains(index)));
    }

    #[test]
    fn unresolved_claim_unit_is_counted_without_a_fabricated_candidate() {
        let candidates = vec![candidate(1, 0, "document:a", true)];
        let selection = select_reference_indices(&candidates, &[Vec::new()], 4);
        assert_eq!(selection.indices, vec![0]);
        assert_eq!(selection.attributed_unit_count, 0);
    }
}
