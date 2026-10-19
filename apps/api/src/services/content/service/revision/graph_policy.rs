use super::*;

const RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT: usize = 12;

pub(super) struct GraphMaterializationSelection {
    pub(super) chunk_count: usize,
    pub(super) selected_graph_chunk_count: usize,
    pub(super) record_stream_source_units_skipped: usize,
    pub(super) graph_chunk_policy: GraphExtractionChunkPolicy,
}
pub(super) fn graph_materialization_selection(
    sources: &GraphMaterializationSources,
) -> GraphMaterializationSelection {
    let graph_chunk_policy = build_graph_extraction_chunk_policy(
        sources.structured_revision.as_ref(),
        sources.all_chunks.as_slice(),
        sources.revision_facts.as_slice(),
    );
    let selected_graph_chunk_count = selected_graph_chunk_count(
        &sources.all_chunks,
        &sources.table_graph_context,
        &graph_chunk_policy,
    );
    GraphMaterializationSelection {
        chunk_count: sources.all_chunks.len(),
        selected_graph_chunk_count,
        record_stream_source_units_skipped: record_stream_source_units_skipped(
            &sources.all_chunks,
            &graph_chunk_policy,
        ),
        graph_chunk_policy,
    }
}

fn selected_graph_chunk_count(
    chunks: &[KnowledgeChunkRow],
    table_graph_context: &RevisionTableGraphContext,
    graph_chunk_policy: &GraphExtractionChunkPolicy,
) -> usize {
    chunks
        .iter()
        .filter(|chunk| {
            build_graph_chunk_content(
                chunk,
                table_graph_context.profile_for_chunk(chunk),
                table_graph_context.requires_row_only_graph(),
                graph_chunk_policy,
            )
            .is_some()
        })
        .count()
}

pub(super) fn log_graph_materialization_selection(
    command: &MaterializeRevisionGraphCandidatesCommand,
    selection: &GraphMaterializationSelection,
) {
    if !selection.graph_chunk_policy.is_record_stream() {
        return;
    }
    tracing::info!(
        revision_id = %command.authority.revision_id,
        selected_graph_chunks = selection.selected_graph_chunk_count,
        selected_source_units = selection.graph_chunk_policy.selected_source_unit_count(),
        skipped_source_units = selection.record_stream_source_units_skipped,
        "record-stream graph extraction chunk policy selected representative source units",
    );
}

fn build_graph_extraction_chunk_policy(
    structured_revision: Option<&KnowledgeStructuredRevisionRow>,
    chunks: &[KnowledgeChunkRow],
    revision_facts: &[TypedTechnicalFact],
) -> GraphExtractionChunkPolicy {
    if !revision_is_record_stream(structured_revision) {
        return GraphExtractionChunkPolicy::standard();
    }
    GraphExtractionChunkPolicy::record_stream(select_record_stream_source_units(
        chunks,
        revision_facts,
    ))
}

fn revision_is_record_stream(structured_revision: Option<&KnowledgeStructuredRevisionRow>) -> bool {
    structured_revision.is_some_and(|revision| revision.source_format == RECORD_JSONL_SOURCE_FORMAT)
}

fn select_record_stream_source_units(
    chunks: &[KnowledgeChunkRow],
    revision_facts: &[TypedTechnicalFact],
) -> BTreeSet<Uuid> {
    let mut source_units = chunks
        .iter()
        .filter(|chunk| chunk.chunk_kind.as_deref() == Some("source_unit"))
        .collect::<Vec<_>>();
    source_units.sort_by_key(|chunk| chunk.chunk_index);
    if source_units.is_empty() {
        return BTreeSet::new();
    }

    let mut selected_indices = BTreeSet::<usize>::new();
    selected_indices.insert(0);
    selected_indices.insert(source_units.len() - 1);

    for (index, chunk) in source_units.iter().enumerate() {
        if selected_indices.len() >= RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT {
            break;
        }
        if revision_facts.iter().any(|fact| typed_fact_supports_chunk(fact, chunk)) {
            selected_indices.insert(index);
        }
    }

    for index in evenly_spaced_source_unit_indices(
        source_units.len(),
        RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT,
    ) {
        if selected_indices.len() >= RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT {
            break;
        }
        selected_indices.insert(index);
    }

    selected_indices
        .into_iter()
        .filter_map(|index| source_units.get(index).map(|chunk| chunk.chunk_id))
        .collect()
}

fn evenly_spaced_source_unit_indices(source_unit_count: usize, limit: usize) -> Vec<usize> {
    if source_unit_count == 0 || limit == 0 {
        return Vec::new();
    }
    if source_unit_count <= limit {
        return (0..source_unit_count).collect();
    }
    if limit == 1 {
        return vec![0];
    }
    (0..limit).map(|slot| slot * (source_unit_count - 1) / (limit - 1)).collect()
}

fn record_stream_source_units_skipped(
    chunks: &[KnowledgeChunkRow],
    policy: &GraphExtractionChunkPolicy,
) -> usize {
    if !policy.is_record_stream() {
        return 0;
    }
    chunks
        .iter()
        .filter(|chunk| chunk.chunk_kind.as_deref() == Some("source_unit"))
        .count()
        .saturating_sub(policy.selected_source_unit_count())
}

#[cfg(test)]
fn test_graph_input_hashes_by_chunk(
    chunks: &[KnowledgeChunkRow],
    table_graph_context: &RevisionTableGraphContext,
) -> HashMap<Uuid, String> {
    let policy = GraphExtractionChunkPolicy::standard();
    chunks
        .iter()
        .filter_map(|chunk| {
            let graph_input = build_graph_chunk_content(
                chunk,
                table_graph_context.profile_for_chunk(chunk),
                table_graph_context.requires_row_only_graph(),
                &policy,
            )?;
            Some((chunk.chunk_id, sha256_hex_text(&graph_input)))
        })
        .collect()
}

#[derive(Clone, Default)]
pub(super) struct RevisionTableGraphContext {
    by_row_block_id: HashMap<Uuid, TableGraphProfile>,
    row_only_table_graph: bool,
}

impl RevisionTableGraphContext {
    pub(super) fn profile_for_chunk(
        &self,
        chunk: &crate::infra::knowledge_rows::KnowledgeChunkRow,
    ) -> Option<&TableGraphProfile> {
        chunk.support_block_ids.iter().find_map(|block_id| self.by_row_block_id.get(block_id))
    }

    pub(super) const fn requires_row_only_graph(&self) -> bool {
        self.row_only_table_graph
    }
}

/// Process-local TTL cache for `load_sub_type_hints_for_extraction`.
///
/// The underlying SQL is a library-wide full scan of
/// `runtime_graph_node` with a JSON-path group-by — measured at
/// ~3.5 s on a mid-size prod corpus under merge load, and the function
/// is called once per
/// `extract_graph` stage (so 1× per document ingested). Under a bulk
/// 24-concurrent worker drain this aggregated to 20+ calls per 30 min
/// window and dominated the slow-statement log after I3 bulk upserts
/// removed the previous merge-side contention. The returned hints
/// change slowly — a single ingest adds at most a handful of
/// (`node_type`, `sub_type`) pairs out of thousands — so a short TTL is
/// sound; readers see at worst a minute-old hint set.
///
/// Cache is keyed by `(library_id, projection_version)` so a new
/// projection version (published at the end of a full graph rebuild)
/// transparently invalidates. Missing/stale entries fall through to
/// the SQL path; SQL failure still yields empty hints, matching the
/// prior fail-open behaviour.
const SUB_TYPE_HINTS_CACHE_TTL: std::time::Duration = std::time::Duration::from_mins(1);
const SUB_TYPE_HINTS_CACHE_CAPACITY: usize = 64;

#[derive(Clone)]
struct SubTypeHintsCacheEntry {
    hints: GraphExtractionSubTypeHints,
    fetched_at: std::time::Instant,
}

type SubTypeHintsCache =
    std::sync::Mutex<std::collections::HashMap<(Uuid, i64), SubTypeHintsCacheEntry>>;

fn sub_type_hints_cache() -> &'static SubTypeHintsCache {
    static CACHE: std::sync::OnceLock<SubTypeHintsCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Deterministically ranks observed hints into the prompt anchor: take the
/// top-N per `node_type` in the repository's `node_type asc, occurrences desc,
/// sub_type asc` order, then sort each group alphabetically so neither the
/// cached hints nor the rendered prompt drift while the chosen set is stable
/// (IRG-197). The occurrence count is a selection rank only; it is never stored
/// or rendered. Pure so the selection can be tested without a database.
fn rank_sub_type_hints(
    rows: Vec<repositories::runtime_graph_repository::RuntimeGraphSubTypeHintRow>,
    top_per_node_type: usize,
) -> GraphExtractionSubTypeHints {
    let mut groups: Vec<GraphExtractionSubTypeHintGroup> = Vec::new();
    for row in rows {
        if groups.last().is_none_or(|group| group.node_type != row.node_type) {
            groups.push(GraphExtractionSubTypeHintGroup {
                node_type: row.node_type.clone(),
                entries: Vec::new(),
            });
        }
        if let Some(group) = groups.last_mut() {
            if group.entries.len() >= top_per_node_type {
                continue;
            }
            group.entries.push(GraphExtractionSubTypeHintEntry { sub_type: row.sub_type });
        }
    }
    for group in &mut groups {
        group.entries.sort_by(|left, right| left.sub_type.cmp(&right.sub_type));
    }
    GraphExtractionSubTypeHints { by_node_type: groups }
}

/// Loads vocabulary-aware extraction hints: observed `sub_type` values per
/// `node_type` for the current library at the active projection version.
///
/// Returns an empty `GraphExtractionSubTypeHints` on any failure (missing
/// snapshot, SQL error, empty graph). Hints are a soft prompt anchor — never
/// fail the ingest path because of them.
pub(super) async fn load_sub_type_hints_for_extraction(
    state: &AppState,
    library_id: Uuid,
) -> GraphExtractionSubTypeHints {
    const TOP_PER_NODE_TYPE: usize = 15;

    let projection_scope = match resolve_projection_scope(state, library_id).await {
        Ok(scope) => scope,
        Err(error) => {
            warn_sub_type_hint_failure(
                library_id,
                &error,
                "sub_type hints: failed to resolve projection scope, falling back to empty hints",
            );
            return GraphExtractionSubTypeHints::default();
        }
    };

    // Cache hit: fresh entry for the same (library, projection_version).
    {
        let guard =
            sub_type_hints_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get(&(library_id, projection_scope.projection_version))
            && entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL
        {
            return entry.hints.clone();
        }
    }

    let rows = match repositories::list_observed_sub_type_hints(
        &state.persistence.postgres,
        library_id,
        projection_scope.projection_version,
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn_sub_type_hint_failure(
                library_id,
                &error,
                "sub_type hints: SQL aggregation failed, falling back to empty hints",
            );
            return GraphExtractionSubTypeHints::default();
        }
    };

    let hints = rank_sub_type_hints(rows, TOP_PER_NODE_TYPE);
    cache_sub_type_hints((library_id, projection_scope.projection_version), &hints);
    hints
}

fn warn_sub_type_hint_failure(library_id: Uuid, error: &impl std::fmt::Display, message: &str) {
    warn!(%library_id, %error, "{message}");
}

fn cache_sub_type_hints(key: (Uuid, i64), hints: &GraphExtractionSubTypeHints) {
    let mut guard =
        sub_type_hints_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.retain(|_, entry| entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL);
    while guard.len() >= SUB_TYPE_HINTS_CACHE_CAPACITY {
        let Some(oldest) =
            guard.iter().min_by_key(|(_, entry)| entry.fetched_at).map(|(key, _)| *key)
        else {
            break;
        };
        guard.remove(&oldest);
    }
    guard.insert(
        key,
        SubTypeHintsCacheEntry { hints: hints.clone(), fetched_at: std::time::Instant::now() },
    );
}

pub(super) fn build_revision_table_graph_context(
    blocks: &[KnowledgeStructuredBlockRow],
) -> RevisionTableGraphContext {
    let mut row_parent_table_ids = HashMap::<Uuid, Uuid>::new();
    let mut summaries_by_table = HashMap::<Uuid, Vec<_>>::new();

    for block in blocks {
        if block.block_kind == "table_row" {
            if let Some(parent_block_id) = block.parent_block_id {
                row_parent_table_ids.insert(block.block_id, parent_block_id);
            }
            continue;
        }

        if block.block_kind != "metadata_block" || !is_table_summary_text(&block.normalized_text) {
            continue;
        }
        let Some(parent_block_id) = block.parent_block_id else {
            continue;
        };
        let Some(summary) = parse_table_column_summary(&block.normalized_text) else {
            continue;
        };
        summaries_by_table.entry(parent_block_id).or_default().push(summary);
    }

    let profiles_by_table = summaries_by_table
        .into_iter()
        .filter_map(|(table_block_id, summaries)| {
            let profile = build_table_graph_profile(&summaries);
            (!profile.is_empty()).then_some((table_block_id, profile))
        })
        .collect::<HashMap<_, _>>();

    let by_row_block_id = row_parent_table_ids
        .into_iter()
        .filter_map(|(row_block_id, table_block_id)| {
            profiles_by_table.get(&table_block_id).cloned().map(|profile| (row_block_id, profile))
        })
        .collect();

    RevisionTableGraphContext {
        by_row_block_id,
        row_only_table_graph: revision_requires_row_only_table_graph(blocks),
    }
}

fn revision_requires_row_only_table_graph(blocks: &[KnowledgeStructuredBlockRow]) -> bool {
    let has_table_rows = blocks.iter().any(|block| block.block_kind == "table_row");
    has_table_rows && blocks.iter().all(block_supports_row_only_table_graph)
}

fn block_supports_row_only_table_graph(block: &KnowledgeStructuredBlockRow) -> bool {
    match block.block_kind.as_str() {
        "heading" | "table" | "table_row" => true,
        "metadata_block" => is_table_summary_text(&block.normalized_text),
        _ => false,
    }
}

#[cfg(test)]
#[path = "graph_policy_tests.rs"]
mod tests;
