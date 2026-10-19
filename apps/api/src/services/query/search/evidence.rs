use crate::infra::knowledge_plane::ChunkLexicalQuery;

use super::*;

/// Merges technical-fact lexical-search passes (the raw query plus one pass
/// per exact-shaped literal from the compiled IR), keeping the
/// highest-scoring row per `fact_id`. Passes are independently ordered by
/// Postgres, so the combined set must be re-sorted rather than concatenated.
pub(super) fn merge_technical_fact_hits(
    passes: impl IntoIterator<Item = Vec<KnowledgeTechnicalFactSearchRow>>,
    limit: usize,
) -> Vec<KnowledgeTechnicalFactSearchRow> {
    let mut merged: HashMap<Uuid, KnowledgeTechnicalFactSearchRow> = HashMap::new();
    for row in passes.into_iter().flatten() {
        merged
            .entry(row.fact_id)
            .and_modify(|existing| {
                if row.score > existing.score {
                    *existing = row.clone();
                }
            })
            .or_insert(row);
    }
    let mut hits: Vec<_> = merged.into_values().collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.fact_id.cmp(&b.fact_id)));
    hits.truncate(limit);
    hits
}

/// Exact-literal technical asks (`QueryIR::is_exact_literal_technical`) name
/// the value verbatim in the question ("Is `/orion/status` valid?", "what
/// does status `500` mean?"). `search_technical_facts`'s exact-match lane
/// binds the *whole* query with whitespace stripped, so it only fires when
/// the raw query text collapses to exactly one fact's normalized value — a
/// multi-word question that merely contains the literal never matches.
/// Run one additional pass per exact-shaped literal from the compiled IR
/// (on top of the raw-query pass, which still covers FTS-only hits) and
/// merge, so the fact search targets the literal Postgres actually indexes
/// against `normalized_value_exact` instead of the surrounding sentence.
async fn search_technical_facts_with_exact_literal_bias(
    state: &AppState,
    library_id: Uuid,
    query: &str,
    query_ir: &QueryIR,
    fact_limit: usize,
) -> std::result::Result<Vec<KnowledgeTechnicalFactSearchRow>, QueryServiceError> {
    let mut passes = vec![
        state
            .search_store
            .search_technical_facts(library_id, query, fact_limit)
            .await
            .context("failed to search normalized technical facts")?,
    ];
    let mut searched_literals: BTreeSet<&str> = BTreeSet::new();
    for literal in query_ir.exact_technical_literals() {
        if !searched_literals.insert(literal) {
            continue;
        }
        passes.push(
            state
                .search_store
                .search_technical_facts(library_id, literal, fact_limit)
                .await
                .context("failed to search normalized technical facts for exact literal")?,
        );
    }
    Ok(merge_technical_fact_hits(passes, fact_limit))
}

impl SearchService {
    pub async fn search_query_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        query: &str,
        query_ir: &QueryIR,
        limit: usize,
    ) -> std::result::Result<QueryEvidenceSearchResult, QueryServiceError> {
        let normalized_limit = limit.max(1);
        // Bias fact retrieval for exact-literal technical asks (known URLs /
        // paths / ports / config keys). Signal comes straight from the
        // compiled IR — `QueryAct::RetrieveValue` with at least one literal
        // constraint — instead of re-scanning the raw query for
        // hand-maintained marker strings.
        let exact_literal_bias = query_ir.is_exact_literal_technical();
        let fact_limit = if exact_literal_bias {
            normalized_limit.saturating_mul(FACT_FETCH_MULTIPLIER).max(FACT_FETCH_MIN)
        } else {
            normalized_limit
        };
        let (temporal_start, temporal_end) = query_ir.resolved_temporal_bounds();
        let chunk_hits = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query,
                query_origin: LexicalQueryOrigin::UserQuery,
                limit: normalized_limit,
                temporal_start,
                temporal_end,
                text_search_config: None,
            })
            .await
            .context("failed to search current knowledge chunks")?;
        let technical_fact_hits = if exact_literal_bias {
            search_technical_facts_with_exact_literal_bias(
                state, library_id, query, query_ir, fact_limit,
            )
            .await?
        } else {
            state
                .search_store
                .search_technical_facts(library_id, query, fact_limit)
                .await
                .context("failed to search normalized technical facts")?
        };
        let entity_hits = state
            .search_store
            .search_entities(library_id, query, normalized_limit)
            .await
            .context("failed to search normalized entities")?;
        let relation_hits = state
            .search_store
            .search_relations(library_id, query, normalized_limit)
            .await
            .context("failed to search normalized relations")?;
        Ok(QueryEvidenceSearchResult {
            chunk_hits,
            technical_fact_hits,
            entity_hits,
            relation_hits,
            exact_literal_bias,
        })
    }
}
