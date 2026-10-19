use anyhow::Context;

use super::{
    AppState, BTreeSet, DOCUMENT_IDENTITY_SCORE_FLOOR, HashMap, HashSet, KnowledgeChunkRow,
    KnowledgeDocumentRow, LATEST_VERSION_CHUNKS_PER_DOCUMENT, LATEST_VERSION_SEMANTIC_CHUNK_CAP,
    LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT, LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT,
    LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP, LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP,
    LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS, LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP,
    LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP, LATEST_VERSION_WINDOW_FETCH_BATCH,
    LatestVersionDocument, LatestVersionSemanticDocument, LatestVersionSemanticRow, QueryIR,
    RetrievalLaneContext, RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid,
    is_source_profile_chunk_row, latest_version_chunk_score, latest_version_documents,
    latest_version_scope_terms, latest_version_semantic_candidate_documents,
    latest_version_semantic_rows_from_chunk_rows,
    latest_version_structural_density_candidate_documents,
    latest_version_structural_inventory_candidate_has_density,
    latest_version_structural_probe_candidate_documents, map_chunk_hit,
    order_latest_version_semantic_rows, query_requests_latest_versions,
    requested_latest_version_count,
};

/// Fetches chunk windows for many revisions in bounded batches and regroups
/// rows per revision. Within a revision, row order matches the single-revision
/// range query (chunk_index ASC, chunk_id ASC), and the per-revision fetch
/// limit is enforced by the store exactly as for single-revision calls.
async fn fetch_chunk_windows_batched(
    state: &AppState,
    windows: &[(Uuid, i32, i32)],
    context_label: &str,
) -> anyhow::Result<HashMap<Uuid, Vec<KnowledgeChunkRow>>> {
    let mut rows_by_revision: HashMap<Uuid, Vec<KnowledgeChunkRow>> = HashMap::new();
    for batch in windows.chunks(LATEST_VERSION_WINDOW_FETCH_BATCH) {
        let rows =
            state.document_store.list_chunks_by_revisions_windows(batch).await.with_context(
                || {
                    format!(
                        "failed to load {context_label} chunk windows for {} revisions",
                        batch.len()
                    )
                },
            )?;
        for row in rows {
            rows_by_revision.entry(row.revision_id).or_default().push(row);
        }
    }
    Ok(rows_by_revision)
}

pub(super) struct LatestVersionSelection {
    pub(super) requested_count: usize,
    pub(super) documents: Vec<LatestVersionDocument>,
}

pub(super) fn select_latest_version_documents(
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> LatestVersionSelection {
    let Some(query_ir) = query_ir.filter(|query_ir| query_requests_latest_versions(query_ir))
    else {
        return LatestVersionSelection { requested_count: 0, documents: Vec::new() };
    };
    let requested_count = requested_latest_version_count(query_ir);
    let scope_terms = latest_version_scope_terms(query_ir);
    LatestVersionSelection {
        requested_count,
        documents: latest_version_documents(document_index, requested_count, &scope_terms),
    }
}

pub(super) async fn load_latest_version_document_chunks(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    requested_count: usize,
    documents: &[LatestVersionDocument],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if requested_count == 0 || documents.is_empty() {
        return Ok(Vec::new());
    }
    let span_started = std::time::Instant::now();

    let windows = documents
        .iter()
        .map(|document| (document.revision_id, 0, LATEST_VERSION_CHUNKS_PER_DOCUMENT as i32))
        .collect::<Vec<_>>();
    let mut rows_by_revision =
        fetch_chunk_windows_batched(state, &windows, "latest-version").await?;
    let mut chunks = Vec::new();
    for (rank, document) in documents.iter().enumerate() {
        let rows = rows_by_revision.remove(&document.revision_id).unwrap_or_default();
        for (chunk_rank, row) in rows
            .into_iter()
            .filter(|row| !is_source_profile_chunk_row(row))
            .take(LATEST_VERSION_CHUNKS_PER_DOCUMENT)
            .enumerate()
        {
            let score = latest_version_chunk_score(
                DOCUMENT_IDENTITY_SCORE_FLOOR,
                requested_count,
                rank,
                chunk_rank,
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
                chunks.push(chunk);
            }
        }
    }
    crate::services::query::turn_spans::record_span(
        "retrieve.latest_version_chunks",
        "db",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        Some(documents.len() as u64),
    );
    Ok(chunks)
}

pub(super) async fn latest_version_semantic_candidates(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    requested_count: usize,
    prefer_source_tail: bool,
    allow_unscoped_density_fallback: bool,
) -> anyhow::Result<Vec<LatestVersionSemanticDocument>> {
    let mut candidates = latest_version_semantic_candidate_documents(
        document_index,
        scope_terms,
        LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    );
    let semantic_document_ids =
        candidates.iter().map(|document| document.document_id).collect::<BTreeSet<_>>();
    let mut candidate_document_ids = semantic_document_ids;
    if prefer_source_tail {
        let mut density_candidates = latest_version_structural_density_candidate_documents(
            state,
            document_index,
            scope_terms,
            &candidate_document_ids,
            LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP,
        )
        .await?;
        candidate_document_ids
            .extend(density_candidates.iter().map(|document| document.document_id));
        if allow_unscoped_density_fallback
            && !scope_terms.is_empty()
            && density_candidates.len() < requested_count.min(4)
        {
            let remaining = LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP
                .saturating_sub(density_candidates.len());
            let unscoped = latest_version_structural_density_candidate_documents(
                state,
                document_index,
                &[],
                &candidate_document_ids,
                remaining,
            )
            .await?;
            candidate_document_ids.extend(unscoped.iter().map(|document| document.document_id));
            density_candidates.extend(unscoped);
        }
        candidates.extend(density_candidates);
    }
    let structural_probe_limit = if scope_terms.is_empty() {
        LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP
    } else {
        LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP
    };
    candidates.extend(latest_version_structural_probe_candidate_documents(
        document_index,
        scope_terms,
        &candidate_document_ids,
        structural_probe_limit,
    ));
    Ok(candidates)
}

pub(super) struct LatestVersionSemanticScan {
    pub(super) rows: Vec<LatestVersionSemanticRow>,
    pub(super) dense_distinct_version_count: usize,
    pub(super) deep_scan_document_count: usize,
}

pub(super) struct DeepScanPlan {
    pub(super) admitted: Vec<bool>,
    pub(super) admitted_count: usize,
}

/// Which candidates get the second, deeper chunk window.
///
/// Admission depends only on the candidate's own flag and on a running cap --
/// never on what the shallow scan returned -- which is what lets both windows
/// be fetched as one batched read instead of a per-document round trip. The
/// cap is the whole point: without it a query over a library of release notes
/// deep-scans every document and the batched read stops being bounded.
pub(super) fn plan_deep_scans(candidates: &[LatestVersionSemanticDocument]) -> DeepScanPlan {
    let mut admitted = vec![false; candidates.len()];
    let mut admitted_count = 0usize;
    for (document_rank, document) in candidates.iter().enumerate() {
        if document.requires_structural_release_density
            && admitted_count < LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP
        {
            admitted[document_rank] = true;
            admitted_count = admitted_count.saturating_add(1);
        }
    }
    DeepScanPlan { admitted, admitted_count }
}

/// Should the structurally dense rows replace the ordinary ones outright?
///
/// They do once enough distinct versions were found to answer a "which
/// versions exist" question. The exception is a bare source-tail request with
/// no scope terms: there the caller wants the end of the document itself, and
/// swapping in version rows answers a question nobody asked.
pub(super) fn dense_rows_should_replace_rows(
    dense_distinct_version_count: usize,
    requested_count: usize,
    scope_terms_empty: bool,
    prefer_source_tail: bool,
) -> bool {
    if scope_terms_empty && prefer_source_tail {
        return false;
    }
    dense_distinct_version_count
        >= requested_count.min(LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS)
}

pub(super) async fn scan_latest_version_semantic_rows(
    state: &AppState,
    candidates: &[LatestVersionSemanticDocument],
    requested_count: usize,
    prefer_source_tail: bool,
    scope_terms: &[String],
) -> anyhow::Result<LatestVersionSemanticScan> {
    let span_started = std::time::Instant::now();

    let DeepScanPlan { admitted: deep_planned, admitted_count: deep_scan_document_count } =
        plan_deep_scans(candidates);
    let shallow_windows = candidates
        .iter()
        .map(|document| (document.revision_id, 0, LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT as i32))
        .collect::<Vec<_>>();
    let mut shallow_rows_by_revision =
        fetch_chunk_windows_batched(state, &shallow_windows, "semantic latest-version").await?;
    let deep_windows = candidates
        .iter()
        .enumerate()
        .filter(|(document_rank, _)| deep_planned[*document_rank])
        .map(|(_, document)| {
            (
                document.revision_id,
                LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT as i32 + 1,
                LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT,
            )
        })
        .collect::<Vec<_>>();
    let mut deep_rows_by_revision =
        fetch_chunk_windows_batched(state, &deep_windows, "semantic latest-version deep").await?;

    let mut rows = Vec::new();
    let mut dense_rows = Vec::new();
    for (document_rank, document) in candidates.iter().enumerate() {
        let chunk_rows = shallow_rows_by_revision.remove(&document.revision_id).unwrap_or_default();
        let (mut candidate_rows, mut distinct_versions, mut structural_score) =
            latest_version_semantic_rows_from_chunk_rows(
                chunk_rows,
                document_rank,
                document.requires_structural_release_density,
            );
        if deep_planned[document_rank] {
            let deep_rows = deep_rows_by_revision.remove(&document.revision_id).unwrap_or_default();
            let (additional_rows, additional_versions, additional_score) =
                latest_version_semantic_rows_from_chunk_rows(
                    deep_rows,
                    document_rank,
                    document.requires_structural_release_density,
                );
            candidate_rows.extend(additional_rows);
            distinct_versions.extend(additional_versions);
            structural_score = structural_score.saturating_add(additional_score);
        }
        if document.requires_structural_release_density
            && !latest_version_structural_inventory_candidate_has_density(
                distinct_versions.len(),
                candidate_rows.len(),
                structural_score,
            )
        {
            continue;
        }
        if document.requires_structural_release_density {
            dense_rows.extend(candidate_rows.into_iter().map(|mut row| {
                row.structural_density_score = structural_score;
                row
            }));
        } else {
            rows.extend(candidate_rows);
        }
    }
    let dense_distinct_version_count =
        dense_rows.iter().map(|row| row.version.clone()).collect::<BTreeSet<_>>().len();
    if dense_rows_should_replace_rows(
        dense_distinct_version_count,
        requested_count,
        scope_terms.is_empty(),
        prefer_source_tail,
    ) {
        rows = dense_rows;
    } else {
        rows.extend(dense_rows);
    }
    crate::services::query::turn_spans::record_span(
        "retrieve.latest_version_scan",
        "db",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        Some(format!("deep_scan_documents={deep_scan_document_count}")),
        Some(candidates.len() as u64),
    );
    Ok(LatestVersionSemanticScan { rows, dense_distinct_version_count, deep_scan_document_count })
}

pub(super) fn latest_version_semantic_chunks_from_rows(
    rows: Vec<LatestVersionSemanticRow>,
    source_tail_inventory: bool,
    requested_count: usize,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> Vec<RuntimeMatchedChunk> {
    let mut chunks = Vec::new();
    let mut seen_versions = BTreeSet::<Vec<u32>>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let limit = requested_count.max(LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    for semantic_row in rows {
        let version_seen =
            !source_tail_inventory && !seen_versions.insert(semantic_row.version.clone());
        if version_seen || !seen_chunk_ids.insert(semantic_row.row.chunk_id) {
            continue;
        }
        let score = latest_version_chunk_score(
            DOCUMENT_IDENTITY_SCORE_FLOOR + 256.0,
            requested_count,
            chunks.len(),
            0,
        );
        if let Some(mut chunk) =
            map_chunk_hit(semantic_row.row, score, document_index, plan_keywords)
        {
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
            chunks.push(chunk);
        }
        if chunks.len() >= limit {
            break;
        }
    }
    chunks.truncate(LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    chunks
}

/// Scope of the latest-version semantic probe for one query: how many rows the
/// explicit latest-version intent asked for and which identity relaxations the
/// probe may fall back to.
pub(super) struct LatestVersionSemanticRequest<'a> {
    pub(super) requested_count: usize,
    pub(super) scope_terms: &'a [String],
    pub(super) explicit_latest_requested: bool,
    pub(super) prefer_source_tail: bool,
    pub(super) allow_unscoped_density_fallback: bool,
}

/// Logs the completed semantic scan without holding it inside the lane body.
fn log_latest_version_semantic_scan(
    candidate_document_count: usize,
    semver_row_count: usize,
    dense_semver_version_count: usize,
    deep_scan_document_count: usize,
    requested_count: usize,
) {
    tracing::info!(
        stage = "retrieval.latest_version_semantic",
        candidate_document_count,
        semver_row_count,
        dense_semver_version_count,
        deep_scan_document_count,
        requested_count,
        "semantic latest-version document scan completed"
    );
}

/// Logs the ordered semantic row selection without holding it inside the lane body.
fn log_latest_version_semantic_selection(
    source_tail_inventory: bool,
    rows: &[LatestVersionSemanticRow],
) {
    let selection_preview = rows
        .iter()
        .take(16)
        .map(|row| {
            format!(
                "{}:{}:{}:{}",
                row.document_rank,
                row.structural_density_score,
                row.row.chunk_index,
                usize::from(row.from_structural_inventory)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    tracing::debug!(
        stage = "retrieval.latest_version_semantic_selection",
        source_tail_inventory,
        selection_preview = %selection_preview,
        "semantic latest-version row selection ordered"
    );
}

pub(super) async fn load_latest_version_semantic_document_chunks(
    context: RetrievalLaneContext<'_>,
    request: LatestVersionSemanticRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext { state, document_index, plan_keywords, .. } = context;
    let LatestVersionSemanticRequest {
        requested_count,
        scope_terms,
        explicit_latest_requested,
        prefer_source_tail,
        allow_unscoped_density_fallback,
    } = request;
    if !explicit_latest_requested || requested_count == 0 {
        return Ok(Vec::new());
    }
    let candidates = latest_version_semantic_candidates(
        state,
        document_index,
        scope_terms,
        requested_count,
        prefer_source_tail,
        allow_unscoped_density_fallback,
    )
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let scan = scan_latest_version_semantic_rows(
        state,
        &candidates,
        requested_count,
        prefer_source_tail,
        scope_terms,
    )
    .await?;
    let mut rows = scan.rows;
    let dense_distinct_version_count = scan.dense_distinct_version_count;
    let deep_scan_document_count = scan.deep_scan_document_count;
    log_latest_version_semantic_scan(
        candidates.len(),
        rows.len(),
        dense_distinct_version_count,
        deep_scan_document_count,
        requested_count,
    );
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let source_tail_inventory = prefer_source_tail;
    rows = order_latest_version_semantic_rows(rows, source_tail_inventory, requested_count);
    log_latest_version_semantic_selection(source_tail_inventory, &rows);

    Ok(latest_version_semantic_chunks_from_rows(
        rows,
        source_tail_inventory,
        requested_count,
        document_index,
        plan_keywords,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP, LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS,
        LatestVersionSemanticDocument, dense_rows_should_replace_rows, plan_deep_scans,
    };
    use uuid::Uuid;

    fn candidate(requires_density: bool) -> LatestVersionSemanticDocument {
        LatestVersionSemanticDocument {
            document_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            semantic_score: 0,
            identity_text: String::new(),
            requires_structural_release_density: requires_density,
        }
    }

    #[test]
    fn plans_no_deep_scan_for_documents_that_do_not_ask_for_one() {
        let plan = plan_deep_scans(&[candidate(false), candidate(false)]);
        assert_eq!(plan.admitted, vec![false, false]);
        assert_eq!(plan.admitted_count, 0);
    }

    #[test]
    fn admits_the_asking_documents_in_rank_order() {
        let plan = plan_deep_scans(&[candidate(false), candidate(true), candidate(true)]);
        assert_eq!(plan.admitted, vec![false, true, true]);
        assert_eq!(plan.admitted_count, 2);
    }

    #[test]
    fn stops_admitting_at_the_cap_so_the_batched_read_stays_bounded() {
        let candidates = vec![candidate(true); LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP + 5];
        let plan = plan_deep_scans(&candidates);
        assert_eq!(plan.admitted_count, LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP);
        assert!(plan.admitted[LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP - 1]);
        assert!(!plan.admitted[LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP]);
        assert_eq!(plan.admitted.len(), candidates.len());
    }

    #[test]
    fn dense_rows_replace_ordinary_rows_once_enough_versions_were_found() {
        let enough = LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS;
        assert!(dense_rows_should_replace_rows(enough, enough, false, false));
        assert!(!dense_rows_should_replace_rows(enough - 1, enough, false, false));
    }

    #[test]
    fn a_scopeless_source_tail_request_keeps_its_own_rows() {
        // "Show me the end of this document" must not be answered with a
        // version list, however many versions the dense scan turned up.
        let plenty = LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS + 10;
        assert!(!dense_rows_should_replace_rows(plenty, 1, true, true));
        assert!(dense_rows_should_replace_rows(plenty, 1, true, false));
        assert!(dense_rows_should_replace_rows(plenty, 1, false, true));
    }

    #[test]
    fn a_small_requested_count_lowers_the_bar() {
        assert!(dense_rows_should_replace_rows(1, 1, false, false));
    }
}
