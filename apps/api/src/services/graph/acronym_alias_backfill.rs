//! Idempotent, per-library recompute of corpus-gloss acronym aliases over
//! existing graph data.
//!
//! Ingest captures acronym aliases at merge time (see
//! [`crate::services::graph::extract::acronym_gloss`]). This pass applies the
//! same structural detector to data already projected, so a library that was
//! ingested before the capture existed gains the aliases without a re-ingest.
//!
//! The pass is **bidirectional**, both directions driven by the same
//! parenthetical-gloss signal:
//!
//! - **Forward** — for each multi-word entity node, re-read the node's evidence
//!   chunk texts (filtered to chunks containing the label phrase), run the
//!   parenthetical-gloss detector, and recompute the initials-shaped alias slot:
//!   stale initials-aliases no longer justified are removed; freshly detected
//!   short forms are added.
//! - **Reverse** — for each node whose label *is* a short identifier-shaped
//!   acronym, re-read its evidence chunk texts and recompute the full-form-phrase
//!   slot: a `<phrase> ( <short> )` gloss found in the node's OWN evidence makes
//!   `<phrase>` an alias. The evidence join is the same-chunk polysemy guard, so a
//!   homograph short node glossed differently elsewhere is untouched.
//!
//! In both directions non-structural aliases (LLM-extracted, manually set) are
//! preserved, and the recompute is idempotent: re-running after a clean pass
//! updates zero rows.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    domains::query_ir::literal_text_is_identifier_shaped,
    infra::repositories,
    services::graph::extract::acronym_gloss::{
        detect_acronym_aliases_for_label, detect_fullform_aliases_for_short_label,
    },
};

/// Number of nodes processed per keyset page. Bounds the working set on a
/// large library while keeping round-trip overhead low.
const NODE_PAGE_SIZE: i64 = 500;

/// Outcome of one backfill pass.
///
/// The forward fields cover the original direction (short acronym attached to a
/// multi-word node); the `reverse_*` fields cover the bidirectional extension
/// (full-form phrase attached to a node whose label is the short acronym).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcronymAliasBackfillReport {
    pub nodes_scanned: usize,
    pub nodes_updated: usize,
    pub aliases_added: usize,
    pub reverse_nodes_scanned: usize,
    pub reverse_nodes_updated: usize,
    pub reverse_aliases_added: usize,
}

/// Runs the acronym-alias recompute for one library over its active projection.
///
/// # Errors
/// Returns an error if the library has no graph projection snapshot or any
/// underlying repository call fails.
pub async fn backfill_acronym_aliases(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<AcronymAliasBackfillReport> {
    let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id)
        .await
        .context("failed to load graph projection snapshot for acronym alias backfill")?
        .ok_or_else(|| anyhow::anyhow!("library {library_id} has no graph projection snapshot"))?;
    let projection_version = snapshot.projection_version;

    let mut report = AcronymAliasBackfillReport::default();
    let mut after_id: Option<Uuid> = None;

    loop {
        let nodes = repositories::list_multiword_runtime_graph_nodes_for_acronym_backfill(
            pool,
            library_id,
            projection_version,
            after_id,
            NODE_PAGE_SIZE,
        )
        .await
        .context("failed to list multi-word nodes for acronym alias backfill")?;
        if nodes.is_empty() {
            break;
        }
        report.nodes_scanned += nodes.len();
        after_id = nodes.last().map(|node| node.id);
        let page_len = nodes.len();

        let node_ids: Vec<Uuid> = nodes.iter().map(|node| node.id).collect();
        let label_by_id: BTreeMap<Uuid, &str> =
            nodes.iter().map(|node| (node.id, node.label.as_str())).collect();

        let evidence = repositories::list_runtime_graph_node_evidence_chunk_texts(
            pool,
            library_id,
            projection_version,
            &node_ids,
        )
        .await
        .context("failed to load node evidence chunk texts for acronym alias backfill")?;

        // Derive the justified (parenthetical-gloss) alias set per node.
        let mut detected: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
        for row in &evidence {
            let Some(label) = label_by_id.get(&row.node_id) else {
                continue;
            };
            for alias in detect_acronym_aliases_for_label(&row.chunk_text, label) {
                detected.entry(row.node_id).or_default().insert(alias);
            }
        }

        // Recompute the initials-slot for every node in the page — even nodes
        // where detection found nothing, so that stale initials-aliases from a
        // previous (now-deleted) detector B pass are removed.
        for node in &nodes {
            let label = node.label.as_str();
            // Derive the uppercased initials that form the discriminating slot.
            let label_initials = compute_label_initials(label);
            let Some(label_initials) = label_initials else {
                // Single-word label: no acronym slot possible, skip.
                continue;
            };
            let justified: Vec<String> =
                detected.get(&node.id).map(|set| set.iter().cloned().collect()).unwrap_or_default();
            let updated = repositories::recompute_runtime_graph_node_structural_aliases(
                pool,
                node.id,
                projection_version,
                &label_initials,
                &justified,
            )
            .await
            .context("failed to recompute acronym aliases for graph node")?;
            if updated > 0 {
                report.nodes_updated += 1;
                report.aliases_added += justified.len();
            }
        }

        if page_len < NODE_PAGE_SIZE as usize {
            break;
        }
    }

    // ── Reverse direction ──────────────────────────────────────────────
    // Attach the full-form phrase to a node whose label IS a short acronym when
    // the `<phrase> ( <short> )` gloss appears in one of the node's OWN evidence
    // chunks. The evidence join in `list_runtime_graph_node_evidence_chunk_texts`
    // is the same-chunk polysemy guard.
    after_id = None;
    loop {
        let nodes = repositories::list_short_token_runtime_graph_nodes_for_acronym_backfill(
            pool,
            library_id,
            projection_version,
            after_id,
            NODE_PAGE_SIZE,
        )
        .await
        .context("failed to list short-token nodes for acronym alias backfill")?;
        if nodes.is_empty() {
            break;
        }
        let page_len = nodes.len();
        after_id = nodes.last().map(|node| node.id);

        // Gate to identifier-shaped acronyms of >= 2 chars before loading any
        // evidence; the SQL pre-filter only removes pure-lowercase prose tokens.
        let acronym_nodes: Vec<_> = nodes
            .iter()
            .filter(|node| {
                let label = node.label.trim();
                literal_text_is_identifier_shaped(label)
                    && label.chars().filter(|ch| ch.is_alphanumeric()).count() >= 2
            })
            .collect();
        report.reverse_nodes_scanned += acronym_nodes.len();

        if !acronym_nodes.is_empty() {
            let node_ids: Vec<Uuid> = acronym_nodes.iter().map(|node| node.id).collect();
            let label_by_id: BTreeMap<Uuid, &str> =
                acronym_nodes.iter().map(|node| (node.id, node.label.as_str())).collect();

            let evidence = repositories::list_runtime_graph_node_evidence_chunk_texts(
                pool,
                library_id,
                projection_version,
                &node_ids,
            )
            .await
            .context("failed to load short-node evidence chunk texts for acronym alias backfill")?;

            // Derive the justified full-form phrases per short node from its own
            // evidence chunks.
            let mut detected: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
            for row in &evidence {
                let Some(label) = label_by_id.get(&row.node_id) else {
                    continue;
                };
                for alias in detect_fullform_aliases_for_short_label(&row.chunk_text, label) {
                    detected.entry(row.node_id).or_default().insert(alias);
                }
            }

            // Union the gloss-justified full forms into each detected node's
            // alias set. ADD-ONLY: unlike the forward direction (which owns the
            // exact short-string slot and may recompute it), full-form aliases
            // have no safe ownership marker, so nothing is ever removed here —
            // see recompute_runtime_graph_node_fullform_aliases.
            for node in &acronym_nodes {
                let Some(justified) =
                    detected.get(&node.id).map(|set| set.iter().cloned().collect::<Vec<String>>())
                else {
                    continue;
                };
                let updated = repositories::recompute_runtime_graph_node_fullform_aliases(
                    pool,
                    node.id,
                    projection_version,
                    &justified,
                )
                .await
                .context("failed to union full-form aliases into short graph node")?;
                if updated > 0 {
                    report.reverse_nodes_updated += 1;
                    report.reverse_aliases_added += justified.len();
                }
            }
        }

        if page_len < NODE_PAGE_SIZE as usize {
            break;
        }
    }

    Ok(report)
}

/// Computes the uppercased per-word initials for a multi-word label.
///
/// Returns `None` for single-word labels or labels whose first token starts
/// with a non-alphabetic character (no clean acronym possible).
fn compute_label_initials(label: &str) -> Option<String> {
    let tokens: Vec<&str> =
        label.split(|ch: char| !ch.is_alphanumeric()).filter(|t| !t.is_empty()).collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut initials = String::new();
    for token in &tokens {
        let first = token.chars().next()?;
        if !first.is_alphabetic() {
            return None;
        }
        initials.extend(first.to_uppercase());
    }
    if initials.chars().count() < 2 {
        return None;
    }
    Some(initials)
}
