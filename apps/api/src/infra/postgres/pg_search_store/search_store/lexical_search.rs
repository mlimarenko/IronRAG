use super::*;

impl PgSearchStore {
    pub(super) async fn search_chunks_impl(
        &self,
        query: &ChunkLexicalQuery<'_>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        if query.query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // `None` borrows the cached default-analyzer SQL unchanged; `Some(config)`
        // renders the lane pair for that config exactly once per call. The default
        // lane is never re-rendered, so cached probes keep zero SQL cost.
        let rendered_config_sql;
        let (sql_exact, sql_prefix): (&str, &str) = match query.text_search_config {
            None => (CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()),
            Some(config) => {
                rendered_config_sql = chunk_lexical_sql(config);
                (rendered_config_sql.0.as_str(), rendered_config_sql.1.as_str())
            }
        };
        let query_terms = lexical_query_terms(query.query);
        let title_ngram_terms = title_ngram_terms(&query_terms);
        let title_identity_terms =
            title_identity_terms(query.query, &query_terms, query.query_origin);
        let title_soft_raw_enabled = title_soft_raw_enabled(&query_terms);
        let exact_identifier_terms = exact_identifier_terms(query.query);
        // Borrow the FTS-independent scoring params once, so the per-pass closure
        // (called for both Pass A and the relaxed passes) only move-captures
        // copyable references, not the owning vectors.
        let params = ChunkLexicalParams {
            query_terms: query_terms.as_slice(),
            title_identity_terms: title_identity_terms.as_slice(),
            title_ngram_terms: title_ngram_terms.as_slice(),
            title_soft_raw_enabled,
            exact_identifier_terms: exact_identifier_terms.as_slice(),
        };
        // Lexical relaxation ladder: precise exact-AND `websearch_to_tsquery`
        // first, relaxed only when sparse. The title-aware scoring params are
        // identical across passes; only the FTS lane (`$2`) widens.
        run_lexical_ladder(
            query.query,
            sql_exact,
            sql_prefix,
            |row: &KnowledgeChunkSearchRow| row.chunk_id,
            |sql, fts_input| async move {
                self.run_chunk_lexical_pass(&sql, &fts_input, query, &params).await
            },
        )
        .await
    }

    pub(super) async fn search_structured_blocks_impl(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let (exact_sql, prefix_sql) = structured_block_lexical_sql();
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeStructuredBlockSearchRow| row.block_id,
            |sql, fts_input| async move {
                let rows =
                    sqlx::query_as::<_, PgStructuredBlockSearchRow>(sqlx::AssertSqlSafe(&*sql))
                        .bind(library_id)
                        .bind(fts_input)
                        .bind(limit.max(1) as i64)
                        .fetch_all(&self.pool)
                        .await
                        .context("failed to search structured blocks")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeStructuredBlockSearchRow {
                        block_id: row.block_id,
                        document_id: row.document_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        revision_id: row.revision_id,
                        ordinal: row.ordinal,
                        block_kind: row.block_kind,
                        text: row.text,
                        normalized_text: row.normalized_text,
                        section_path: row.section_path,
                        heading_trail: row.heading_trail,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }

    pub(super) async fn search_technical_facts_impl(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query_exact = query.split_whitespace().collect::<String>();
        // `$3` (exact normalized value) and `$4` (limit) are FTS-independent, so
        // they bind identically across passes; only the FTS lane (`$2`) widens.
        let (exact_sql, prefix_sql) = technical_fact_lexical_sql();
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeTechnicalFactSearchRow| row.fact_id,
            |sql, fts_input| {
                let query_exact = query_exact.clone();
                async move {
                    let rows =
                        sqlx::query_as::<_, PgTechnicalFactSearchRow>(sqlx::AssertSqlSafe(&*sql))
                            .bind(library_id)
                            .bind(fts_input)
                            .bind(query_exact)
                            .bind(limit.max(1) as i64)
                            .fetch_all(&self.pool)
                            .await
                            .context("failed to search technical facts")?;
                    Ok(rows
                        .into_iter()
                        .map(|row| KnowledgeTechnicalFactSearchRow {
                            fact_id: row.fact_id,
                            document_id: row.document_id,
                            workspace_id: row.workspace_id,
                            library_id: row.library_id,
                            revision_id: row.revision_id,
                            fact_kind: row.fact_kind,
                            normalized_value_text: row.normalized_value_text,
                            display_value: row.display_value,
                            exact_match: row.exact_match,
                            score: row.score,
                        })
                        .collect())
                }
            },
        )
        .await
    }

    pub(super) async fn search_entities_impl(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntitySearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let visibility = current_knowledge_graph_target!(
            "knowledge_evidence_entity_support",
            "e.entity_id",
            "e.library_id"
        );
        let (exact_sql, prefix_sql) = lexical_lane_sql(&format!(
            "select e.entity_id, e.workspace_id, e.library_id, e.\u{63}anonical_label as primary_label, e.entity_type, e.summary,
                ts_rank_cd(e.search_tsv, {{FTS}})::double precision as score
             from knowledge_entity e
             where e.library_id = $1
                and e.entity_state = 'active'
                and {visibility}
                and e.search_tsv @@ {{FTS}}
             order by score desc, e.entity_id asc
             limit $3",
        ));
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeEntitySearchRow| row.entity_id,
            |sql, fts_input| async move {
                let rows = sqlx::query_as::<_, PgEntitySearchRow>(sqlx::AssertSqlSafe(&*sql))
                    .bind(library_id)
                    .bind(fts_input)
                    .bind(limit.max(1) as i64)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to search knowledge entities")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeEntitySearchRow {
                        entity_id: row.entity_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        primary_label: row.primary_label,
                        entity_type: row.entity_type,
                        summary: row.summary,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }

    pub(super) async fn search_relations_impl(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let visibility = current_knowledge_graph_target!(
            "knowledge_evidence_relation_support",
            "r.relation_id",
            "r.library_id"
        );
        let (exact_sql, prefix_sql) = lexical_lane_sql(&format!(
            "select relation_id, workspace_id, library_id, predicate, normalized_assertion, summary,
                ts_rank_cd(search_tsv, {{FTS}})::double precision as score
             from knowledge_relation r
             where library_id = $1
                and relation_state = 'active'
                and {visibility}
                and search_tsv @@ {{FTS}}
             order by score desc, relation_id asc
             limit $3",
        ));
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeRelationSearchRow| row.relation_id,
            |sql, fts_input| async move {
                let rows = sqlx::query_as::<_, PgRelationSearchRow>(sqlx::AssertSqlSafe(&*sql))
                    .bind(library_id)
                    .bind(fts_input)
                    .bind(limit.max(1) as i64)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to search knowledge relations")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeRelationSearchRow {
                        relation_id: row.relation_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        predicate: row.predicate,
                        normalized_assertion: row.normalized_assertion,
                        summary: row.summary,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }
}
