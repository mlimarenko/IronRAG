use super::*;

use std::sync::OnceLock;

use opentelemetry::{KeyValue, global, metrics::Counter};

struct ChunkVectorSearchSetup {
    relation_name: String,
    manifest_row_count: u64,
    relation: String,
    query_literal: String,
    cast_type: &'static str,
    search_params: PgHnswSearchParams,
}

impl ChunkVectorSearchSetup {
    /// Emit the single underfill decision record for the primary chunk lane.
    /// Keeping it here holds the tracing field set identical in the retried
    /// and skipped branches without nesting both inside the caller.
    fn log_chunk_underfill(
        &self,
        library_id: Uuid,
        requested_limit: usize,
        underfill: &AnnUnderfill,
        exact_result_count: usize,
    ) {
        if underfill.retry {
            self.log_chunk_exact_retry(library_id, requested_limit, underfill, exact_result_count);
        } else {
            self.log_chunk_exact_retry_skipped(library_id, requested_limit, underfill);
        }
    }

    fn log_chunk_exact_retry(
        &self,
        library_id: Uuid,
        requested_limit: usize,
        underfill: &AnnUnderfill,
        exact_result_count: usize,
    ) {
        let params = self.search_params;
        tracing::warn!(
            library_id = %library_id,
            approximate_result_count = underfill.approximate_result_count,
            exact_result_count,
            requested_limit,
            manifest_row_count = self.manifest_row_count,
            ef_search = params.ef_search,
            max_scan_tuples = params.max_scan_tuples,
            scan_mem_multiplier = params.scan_mem_multiplier,
            "filtered HNSW chunk search underfilled and used a bounded exact retry"
        );
    }

    fn log_chunk_exact_retry_skipped(
        &self,
        library_id: Uuid,
        requested_limit: usize,
        underfill: &AnnUnderfill,
    ) {
        let params = self.search_params;
        tracing::warn!(
            library_id = %library_id,
            approximate_result_count = underfill.approximate_result_count,
            requested_limit,
            manifest_row_count = self.manifest_row_count,
            exact_fallback_max_rows = underfill.exact_fallback_max_rows,
            ef_search = params.ef_search,
            max_scan_tuples = params.max_scan_tuples,
            scan_mem_multiplier = params.scan_mem_multiplier,
            "filtered HNSW chunk search underfilled at the configured scan bound; exact retry skipped"
        );
    }
}

struct ChunkVectorQuery<'a> {
    library_id: Uuid,
    embedding_model_key: &'a str,
    temporal_start: Option<&'a DateTime<Utc>>,
    temporal_end: Option<&'a DateTime<Utc>>,
    limit: i64,
}

/// Decision and diagnostic inputs for a bounded exact retry of an ANN lane.
struct AnnUnderfill {
    approximate_result_count: usize,
    exact_fallback_max_rows: u64,
    retry: bool,
}

impl AnnUnderfill {
    fn classify(
        approximate_result_count: usize,
        requested_limit: usize,
        manifest_row_count: u64,
        exact_fallback_max_rows: u64,
    ) -> Self {
        Self {
            approximate_result_count,
            exact_fallback_max_rows,
            retry: should_retry_exact_ann(
                approximate_result_count,
                requested_limit,
                manifest_row_count,
                exact_fallback_max_rows,
            ),
        }
    }
}

impl ChunkVectorQuery<'_> {
    async fn fetch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        setup: &ChunkVectorSearchSetup,
        exact: bool,
    ) -> anyhow::Result<Vec<PgChunkVectorSearchRow>> {
        let sql = if exact {
            chunk_vector_exact_similarity_sql(&setup.relation, setup.cast_type)
        } else {
            chunk_vector_similarity_sql(&setup.relation, setup.cast_type)
        };
        sqlx::query_as::<_, PgChunkVectorSearchRow>(sqlx::AssertSqlSafe(sql))
            .bind(self.library_id)
            .bind(self.embedding_model_key)
            .bind(&setup.query_literal)
            .bind(self.temporal_start)
            .bind(self.temporal_end)
            .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
            .bind(self.limit)
            .fetch_all(&mut **tx)
            .await
            .with_context(|| format!("failed to search chunk vectors in {}", setup.relation_name))
    }
}

fn document_vector_rescue_counter() -> &'static Counter<u64> {
    static COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    COUNTER.get_or_init(|| {
        global::meter("ironrag.search")
            .u64_counter("ironrag.search.chunk_vector.document_rescue")
            .with_description("Document-diversity vector rescue decisions")
            .with_unit("{decision}")
            .build()
    })
}

fn record_document_vector_rescue_decision(decision: &'static str) {
    document_vector_rescue_counter().add(1, &[KeyValue::new("decision", decision)]);
}

/// Record and explain the decision to skip the exact document-diversity rescue
/// because the configured scan bound forbids it. Extracted so the caller's
/// rescue branch keeps one nesting level and the decision vocabulary lives in
/// one place.
fn record_skipped_document_vector_rescue(
    library_id: Uuid,
    represented_document_count: usize,
    requested_document_limit: usize,
    manifest_row_count: u64,
    exact_fallback_max_rows: u64,
) {
    let decision =
        if exact_fallback_max_rows == 0 { "skipped_disabled" } else { "skipped_manifest_bound" };
    record_document_vector_rescue_decision(decision);
    tracing::warn!(
        library_id = %library_id,
        represented_document_count,
        requested_document_limit,
        manifest_row_count,
        exact_fallback_max_rows,
        decision,
        "exact document-diversity vector rescue skipped at the configured scan bound; returning bounded ANN candidates"
    );
}

impl PgSearchStore {
    pub(super) async fn search_chunk_vectors_by_similarity_impl(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        let library_id = input.library_id;
        let embedding_model_key = input.embedding_model_key;
        let Some(setup) = self.resolve_chunk_vector_search_setup(&input).await? else {
            return Ok(Vec::new());
        };
        let query_limit = limit.max(1);
        let query_limit_i64 =
            i64::try_from(query_limit).context("chunk ANN limit overflowed i64")?;
        let mut tx = self.pool.begin().await?;
        configure_hnsw_search(&mut tx, setup.search_params).await?;
        force_custom_chunk_vector_plan(&mut tx).await?;
        let query = ChunkVectorQuery {
            library_id,
            embedding_model_key,
            temporal_start: temporal_start.as_ref(),
            temporal_end: temporal_end.as_ref(),
            limit: query_limit_i64,
        };
        let mut rows = query.fetch(&mut tx, &setup, false).await?;
        if ann_search_underfilled(rows.len(), query_limit, setup.manifest_row_count) {
            let underfill = AnnUnderfill::classify(
                rows.len(),
                query_limit,
                setup.manifest_row_count,
                pg_hnsw_exact_fallback_max_rows(),
            );
            if underfill.retry {
                rows = query.fetch(&mut tx, &setup, true).await.with_context(|| {
                    format!("failed bounded exact chunk-vector retry in {}", setup.relation_name)
                })?;
            }
            setup.log_chunk_underfill(library_id, query_limit, &underfill, rows.len());
        }
        tx.commit().await?;
        Ok(rows.into_iter().map(chunk_vector_search_row).collect())
    }

    pub(super) async fn search_chunk_vectors_by_document_similarity_impl(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        document_limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        let library_id = input.library_id;
        let embedding_model_key = input.embedding_model_key;
        let Some(setup) = self.resolve_chunk_vector_search_setup(&input).await? else {
            return Ok(Vec::new());
        };
        let document_limit = document_limit.max(1);
        let ann_limit = document_limit.saturating_mul(2);
        let ann_limit_i64 = i64::try_from(ann_limit).context("chunk ANN limit overflowed i64")?;
        let mut tx = self.pool.begin().await?;
        configure_hnsw_search(&mut tx, setup.search_params).await?;
        let document_query = ChunkVectorQuery {
            limit: ann_limit_i64,
            temporal_end: temporal_end.as_ref(),
            temporal_start: temporal_start.as_ref(),
            embedding_model_key,
            library_id,
        };
        let mut ann_rows =
            document_query.fetch(&mut tx, &setup, false).await.with_context(|| {
                format!("failed to search document-dense chunk vectors in {}", setup.relation_name)
            })?;
        if ann_search_underfilled(ann_rows.len(), ann_limit, setup.manifest_row_count)
            && AnnUnderfill::classify(
                ann_rows.len(),
                ann_limit,
                setup.manifest_row_count,
                pg_hnsw_exact_fallback_max_rows(),
            )
            .retry
        {
            ann_rows = document_query.fetch(&mut tx, &setup, true).await.with_context(|| {
                format!("failed exact document-dense chunk-vector retry in {}", setup.relation_name)
            })?;
        }

        let mut best_by_document = best_chunk_vector_rows_by_document(ann_rows);
        let exact_fallback_max_rows = pg_hnsw_exact_fallback_max_rows();
        if should_run_exact_document_rescue(
            best_by_document.len(),
            document_limit,
            setup.manifest_row_count,
            exact_fallback_max_rows,
        ) {
            record_document_vector_rescue_decision("executed");
            let represented_document_ids = best_by_document.keys().copied().collect::<Vec<Uuid>>();
            let rescue_limit = document_limit.saturating_sub(best_by_document.len());
            let rescue_limit_i64 =
                i64::try_from(rescue_limit).context("chunk vector rescue limit overflowed i64")?;
            let rescue_rows = sqlx::query_as::<_, PgChunkVectorSearchRow>(sqlx::AssertSqlSafe(
                chunk_vector_document_rescue_sql(&setup.relation, setup.cast_type),
            ))
            .bind(library_id)
            .bind(embedding_model_key)
            .bind(&setup.query_literal)
            .bind(temporal_start.as_ref())
            .bind(temporal_end.as_ref())
            .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
            .bind(&represented_document_ids)
            .bind(rescue_limit_i64)
            .fetch_all(&mut *tx)
            .await
            .with_context(|| {
                format!("failed exact unseen-document vector rescue in {}", setup.relation_name)
            })?;
            for row in rescue_rows {
                best_by_document.entry(row.document_id).or_insert(row);
            }
        } else if best_by_document.len() < document_limit
            && u64::try_from(best_by_document.len())
                .is_ok_and(|represented| setup.manifest_row_count > represented)
        {
            record_skipped_document_vector_rescue(
                library_id,
                best_by_document.len(),
                document_limit,
                setup.manifest_row_count,
                exact_fallback_max_rows,
            );
        }
        tx.commit().await?;

        let mut rows = best_by_document.into_values().collect::<Vec<_>>();
        rows.sort_by(chunk_vector_document_order);
        rows.truncate(document_limit);
        Ok(rows.into_iter().map(chunk_vector_search_row).collect())
    }

    async fn resolve_chunk_vector_search_setup(
        &self,
        input: &VectorSimilaritySearchInput<'_>,
    ) -> anyhow::Result<Option<ChunkVectorSearchSetup>> {
        validate_query_vector_dimensions(input.dim, input.query_vector, "chunk")?;
        let Some(search_lane) = self
            .resolve_manifest_search_lane(
                input.library_id,
                input.dim,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
                input.embedding_model_key,
                CHUNK_VECTOR_RELATION_PREFIX,
            )
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(ChunkVectorSearchSetup {
            relation: quote_identifier(&search_lane.relation_name)?,
            relation_name: search_lane.relation_name,
            manifest_row_count: search_lane.manifest_row_count,
            query_literal: pgvector_literal(input.query_vector)?,
            cast_type: PgVectorStorage::for_dim(input.dim).cast_type(),
            search_params: pg_hnsw_search_params(input.n_probe),
        }))
    }

    pub(super) async fn search_entity_vectors_by_similarity_impl(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>> {
        let VectorSimilaritySearchInput {
            dim,
            library_id,
            embedding_model_key,
            query_vector,
            n_probe,
        } = input;
        validate_query_vector_dimensions(dim, query_vector, "entity")?;
        let Some(search_lane) = self
            .resolve_manifest_search_lane(
                library_id,
                dim,
                KNOWLEDGE_ENTITY_VECTOR_KIND,
                embedding_model_key,
                ENTITY_VECTOR_RELATION_PREFIX,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let relation_name = search_lane.relation_name;
        let manifest_row_count = search_lane.manifest_row_count;
        let relation = quote_identifier(&relation_name)?;
        let query_literal = pgvector_literal(query_vector)?;
        let storage = PgVectorStorage::for_dim(dim);
        let cast_type = storage.cast_type();
        let search_params = pg_hnsw_search_params(n_probe);
        let query_limit = limit.max(1);
        let query_limit_i64 =
            i64::try_from(query_limit).context("entity ANN limit overflowed i64")?;
        let mut tx = self.pool.begin().await?;
        configure_hnsw_search(&mut tx, search_params).await?;
        let mut rows = sqlx::query_as::<_, PgEntityVectorSearchRow>(sqlx::AssertSqlSafe(
            entity_vector_similarity_sql(&relation, cast_type),
        ))
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(&query_literal)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .bind(query_limit_i64)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("failed to search entity vectors in {relation_name}"))?;
        if ann_search_underfilled(rows.len(), query_limit, manifest_row_count) {
            let underfill = AnnUnderfill::classify(
                rows.len(),
                query_limit,
                manifest_row_count,
                pg_hnsw_exact_fallback_max_rows(),
            );
            if underfill.retry {
                rows = sqlx::query_as::<_, PgEntityVectorSearchRow>(sqlx::AssertSqlSafe(
                    entity_vector_exact_similarity_sql(&relation, cast_type),
                ))
                .bind(library_id)
                .bind(embedding_model_key)
                .bind(&query_literal)
                .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
                .bind(query_limit_i64)
                .fetch_all(&mut *tx)
                .await
                .with_context(|| {
                    format!("failed bounded exact entity-vector retry in {relation_name}")
                })?;
            }
            log_entity_underfill(
                library_id,
                query_limit,
                manifest_row_count,
                search_params,
                &underfill,
                rows.len(),
            );
        }
        tx.commit().await?;
        Ok(rows.into_iter().map(entity_vector_search_row).collect())
    }
}

/// Emit the single underfill decision record for the entity ANN lane. Kept
/// beside the search so the retried and skipped branches log an identical
/// field set without nesting both inside the caller.
fn log_entity_underfill(
    library_id: Uuid,
    requested_limit: usize,
    manifest_row_count: u64,
    search_params: PgHnswSearchParams,
    underfill: &AnnUnderfill,
    exact_result_count: usize,
) {
    if underfill.retry {
        log_entity_exact_retry(
            library_id,
            requested_limit,
            manifest_row_count,
            search_params,
            underfill,
            exact_result_count,
        );
    } else {
        log_entity_exact_retry_skipped(
            library_id,
            requested_limit,
            manifest_row_count,
            search_params,
            underfill,
        );
    }
}

fn log_entity_exact_retry(
    library_id: Uuid,
    requested_limit: usize,
    manifest_row_count: u64,
    search_params: PgHnswSearchParams,
    underfill: &AnnUnderfill,
    exact_result_count: usize,
) {
    tracing::warn!(
        library_id = %library_id,
        approximate_result_count = underfill.approximate_result_count,
        exact_result_count,
        requested_limit,
        manifest_row_count,
        ef_search = search_params.ef_search,
        max_scan_tuples = search_params.max_scan_tuples,
        scan_mem_multiplier = search_params.scan_mem_multiplier,
        "filtered HNSW entity search underfilled and used a bounded exact retry"
    );
}

fn log_entity_exact_retry_skipped(
    library_id: Uuid,
    requested_limit: usize,
    manifest_row_count: u64,
    search_params: PgHnswSearchParams,
    underfill: &AnnUnderfill,
) {
    tracing::warn!(
        library_id = %library_id,
        approximate_result_count = underfill.approximate_result_count,
        requested_limit,
        manifest_row_count,
        exact_fallback_max_rows = underfill.exact_fallback_max_rows,
        ef_search = search_params.ef_search,
        max_scan_tuples = search_params.max_scan_tuples,
        scan_mem_multiplier = search_params.scan_mem_multiplier,
        "filtered HNSW entity search underfilled at the configured scan bound; exact retry skipped"
    );
}

fn entity_vector_search_row(row: PgEntityVectorSearchRow) -> KnowledgeEntityVectorSearchRow {
    KnowledgeEntityVectorSearchRow {
        vector_id: row.vector_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        entity_id: row.entity_id,
        embedding_model_key: row.embedding_model_key,
        vector_kind: row.vector_kind,
        freshness_generation: row.freshness_generation,
        score: row.score,
    }
}

fn best_chunk_vector_rows_by_document(
    rows: Vec<PgChunkVectorSearchRow>,
) -> BTreeMap<Uuid, PgChunkVectorSearchRow> {
    let mut best_by_document = BTreeMap::new();
    for row in rows {
        match best_by_document.get_mut(&row.document_id) {
            Some(best) if chunk_vector_document_order(&row, best).is_lt() => *best = row,
            Some(_) => {}
            None => {
                best_by_document.insert(row.document_id, row);
            }
        }
    }
    best_by_document
}

fn chunk_vector_document_order(
    left: &PgChunkVectorSearchRow,
    right: &PgChunkVectorSearchRow,
) -> std::cmp::Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.document_id.cmp(&right.document_id))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

fn chunk_vector_search_row(row: PgChunkVectorSearchRow) -> KnowledgeChunkVectorSearchRow {
    KnowledgeChunkVectorSearchRow {
        vector_id: row.vector_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        chunk_id: row.chunk_id,
        revision_id: row.revision_id,
        embedding_model_key: row.embedding_model_key,
        vector_kind: row.vector_kind,
        freshness_generation: row.freshness_generation,
        score: row.score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ann_underfill_keeps_scan_bound_and_original_result_count() {
        for (found, requested, manifest, bound, retry) in [
            (3, 8, 1_000, 10_000, true),
            (8, 8, 1_000, 10_000, false),
            (3, 8, 7, 10_000, false),
            (3, 8, 10_001, 10_000, false),
            (3, 8, 1_000, 0, false),
        ] {
            let decision = AnnUnderfill::classify(found, requested, manifest, bound);
            assert_eq!(decision.retry, retry);
            assert_eq!(decision.approximate_result_count, found);
            assert_eq!(decision.exact_fallback_max_rows, bound);
        }
    }

    #[test]
    fn entity_search_row_preserves_scope_profile_freshness_and_score() {
        let row = PgEntityVectorSearchRow {
            vector_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            entity_id: Uuid::from_u128(4),
            embedding_model_key: "profile-seven".into(),
            vector_kind: "entity".into(),
            freshness_generation: 43,
            score: 0.75,
        };
        let mapped = entity_vector_search_row(row.clone());
        assert_eq!(mapped.vector_id, row.vector_id);
        assert_eq!(mapped.workspace_id, row.workspace_id);
        assert_eq!(mapped.library_id, row.library_id);
        assert_eq!(mapped.entity_id, row.entity_id);
        assert_eq!(mapped.embedding_model_key, row.embedding_model_key);
        assert_eq!(mapped.vector_kind, row.vector_kind);
        assert_eq!(mapped.freshness_generation, row.freshness_generation);
        assert_eq!(mapped.score, row.score);
    }
}
