//! Idempotent, per-library recompute of corpus-gloss acronym aliases over
//! existing graph data.
//!
//! Ingest captures acronym aliases at merge time (see
//! `crate::services::graph::extract::acronym_gloss`). This pass applies the
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
    let projection_version = acronym_backfill_projection_version(pool, library_id).await?;
    let forward = backfill_forward_aliases(pool, library_id, projection_version).await?;
    let reverse = backfill_reverse_aliases(pool, library_id, projection_version).await?;

    Ok(AcronymAliasBackfillReport {
        nodes_scanned: forward.nodes_scanned,
        nodes_updated: forward.nodes_updated,
        aliases_added: forward.aliases_added,
        reverse_nodes_scanned: reverse.reverse_nodes_scanned,
        reverse_nodes_updated: reverse.reverse_nodes_updated,
        reverse_aliases_added: reverse.reverse_aliases_added,
    })
}

async fn acronym_backfill_projection_version(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    repositories::get_runtime_graph_snapshot(pool, library_id)
        .await
        .context("failed to load graph projection snapshot for acronym alias backfill")?
        .map(|snapshot| snapshot.projection_version)
        .ok_or_else(|| anyhow::anyhow!("library {library_id} has no graph projection snapshot"))
}

async fn backfill_forward_aliases(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<AcronymAliasBackfillReport> {
    let mut report = AcronymAliasBackfillReport::default();
    let mut after_id = None;

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
        let Some(last_node_id) = nodes.last().map(|node| node.id) else {
            break;
        };
        let page_len = nodes.len();
        report.nodes_scanned += page_len;
        let detected = detect_forward_aliases(pool, library_id, projection_version, &nodes).await?;
        let counts = recompute_forward_aliases(pool, projection_version, &nodes, &detected).await?;
        report.nodes_updated += counts.nodes_updated;
        report.aliases_added += counts.aliases_added;

        if page_len < NODE_PAGE_SIZE as usize {
            break;
        }
        after_id = Some(last_node_id);
    }

    Ok(report)
}

async fn detect_forward_aliases(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
    nodes: &[repositories::AcronymBackfillNodeRow],
) -> anyhow::Result<BTreeMap<Uuid, BTreeSet<String>>> {
    let node_ids = nodes.iter().map(|node| node.id).collect::<Vec<_>>();
    let label_by_id =
        nodes.iter().map(|node| (node.id, node.label.as_str())).collect::<BTreeMap<_, _>>();
    let evidence = repositories::list_runtime_graph_node_evidence_chunk_texts(
        pool,
        library_id,
        projection_version,
        &node_ids,
    )
    .await
    .context("failed to load node evidence chunk texts for acronym alias backfill")?;

    Ok(collect_detected_aliases(&evidence, &label_by_id, detect_acronym_aliases_for_label))
}

fn collect_detected_aliases(
    evidence: &[repositories::NodeEvidenceChunkTextRow],
    label_by_id: &BTreeMap<Uuid, &str>,
    detect_aliases: impl Fn(&str, &str) -> Vec<String>,
) -> BTreeMap<Uuid, BTreeSet<String>> {
    let mut detected: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for row in evidence {
        let Some(label) = label_by_id.get(&row.node_id) else {
            continue;
        };
        for alias in detect_aliases(&row.chunk_text, label) {
            detected.entry(row.node_id).or_default().insert(alias);
        }
    }
    detected
}

#[derive(Default)]
struct AliasBackfillCounts {
    nodes_updated: usize,
    aliases_added: usize,
}

async fn recompute_forward_aliases(
    pool: &sqlx::PgPool,
    projection_version: i64,
    nodes: &[repositories::AcronymBackfillNodeRow],
    detected: &BTreeMap<Uuid, BTreeSet<String>>,
) -> anyhow::Result<AliasBackfillCounts> {
    let mut counts = AliasBackfillCounts::default();
    for node in nodes {
        let Some(label_initials) = compute_label_initials(&node.label) else {
            continue;
        };
        let justified = detected
            .get(&node.id)
            .map_or_else(Vec::new, |aliases| aliases.iter().cloned().collect());
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
            counts.nodes_updated += 1;
            counts.aliases_added += justified.len();
        }
    }
    Ok(counts)
}

async fn backfill_reverse_aliases(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<AcronymAliasBackfillReport> {
    let mut report = AcronymAliasBackfillReport::default();
    let mut after_id = None;

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
        let Some(last_node_id) = nodes.last().map(|node| node.id) else {
            break;
        };
        let page_len = nodes.len();
        let acronym_nodes = nodes
            .iter()
            .filter(|node| is_eligible_reverse_acronym_label(&node.label))
            .collect::<Vec<_>>();
        report.reverse_nodes_scanned += acronym_nodes.len();
        let counts =
            recompute_reverse_aliases(pool, library_id, projection_version, &acronym_nodes).await?;
        report.reverse_nodes_updated += counts.nodes_updated;
        report.reverse_aliases_added += counts.aliases_added;

        if page_len < NODE_PAGE_SIZE as usize {
            break;
        }
        after_id = Some(last_node_id);
    }

    Ok(report)
}

fn is_eligible_reverse_acronym_label(label: &str) -> bool {
    let label = label.trim();
    literal_text_is_identifier_shaped(label)
        && label.chars().filter(|character| character.is_alphanumeric()).count() >= 2
}

async fn recompute_reverse_aliases(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    projection_version: i64,
    nodes: &[&repositories::AcronymBackfillNodeRow],
) -> anyhow::Result<AliasBackfillCounts> {
    if nodes.is_empty() {
        return Ok(AliasBackfillCounts::default());
    }
    let node_ids = nodes.iter().map(|node| node.id).collect::<Vec<_>>();
    let label_by_id =
        nodes.iter().map(|node| (node.id, node.label.as_str())).collect::<BTreeMap<_, _>>();
    let evidence = repositories::list_runtime_graph_node_evidence_chunk_texts(
        pool,
        library_id,
        projection_version,
        &node_ids,
    )
    .await
    .context("failed to load short-node evidence chunk texts for acronym alias backfill")?;
    let detected =
        collect_detected_aliases(&evidence, &label_by_id, detect_fullform_aliases_for_short_label);

    let mut counts = AliasBackfillCounts::default();
    for node in nodes {
        let Some(justified) =
            detected.get(&node.id).map(|aliases| aliases.iter().cloned().collect::<Vec<_>>())
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
            counts.nodes_updated += 1;
            counts.aliases_added += justified.len();
        }
    }

    Ok(counts)
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

#[cfg(test)]
mod tests {
    use super::compute_label_initials;

    #[test]
    fn computes_uppercase_initials_for_multiword_labels() {
        assert_eq!(compute_label_initials("alpha-suite connector"), Some("ASC".to_string()));
        assert_eq!(compute_label_initials("single"), None);
        assert_eq!(compute_label_initials("42 alpha"), None);
    }
}
