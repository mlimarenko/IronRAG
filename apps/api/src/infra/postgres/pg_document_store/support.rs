use super::*;

const STRUCTURED_BLOCK_INSERT_BIND_COUNT: usize = 19;
const STRUCTURED_BLOCK_INSERT_BATCH_ROWS: usize = 1_000;
const TECHNICAL_FACT_INSERT_BIND_COUNT: usize = 18;
const TECHNICAL_FACT_INSERT_BATCH_ROWS: usize = 1_000;
const POSTGRES_MAX_BIND_PARAMETERS: usize = 65_535;

pub(super) async fn lock_ingest_revision_write_authority(
    transaction: &mut Transaction<'_, Postgres>,
    authority: IngestRevisionWriteAuthority,
) -> anyhow::Result<bool> {
    if !catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        authority.workspace_id,
        authority.library_id,
    )
    .await
    .context("failed to lock library for ingest revision mutation")?
    {
        return Ok(false);
    }
    ingest_repository::lock_latest_leased_revision_attempt(
        transaction,
        authority.library_id,
        authority.attempt_id,
        authority.revision_id,
    )
    .await
    .context("failed to lock latest leased ingest revision attempt")
}

pub(super) async fn replace_structured_revision_artifact_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    row: &KnowledgeStructuredRevisionRow,
    blocks: &[KnowledgeStructuredBlockRow],
) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
    let revision_scope = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "SELECT workspace_id, library_id, document_id
         FROM knowledge_revision
         WHERE revision_id = $1
         FOR UPDATE",
    )
    .bind(row.revision_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to lock structured artifact revision")?
    .context("structured artifact revision does not exist")?;

    anyhow::ensure!(
        row.workspace_id == revision_scope.0
            && row.library_id == revision_scope.1
            && row.document_id == revision_scope.2,
        "structured revision metadata does not match its authoritative revision scope"
    );
    let declared_block_count = usize::try_from(row.block_count)
        .context("structured revision block count must be non-negative")?;
    usize::try_from(row.chunk_count)
        .context("structured revision chunk count must be non-negative")?;
    usize::try_from(row.typed_fact_count)
        .context("structured revision typed fact count must be non-negative")?;
    anyhow::ensure!(
        declared_block_count == blocks.len(),
        "structured revision declares {} blocks but replacement contains {}",
        row.block_count,
        blocks.len()
    );
    anyhow::ensure!(
        blocks.iter().all(|block| {
            block.revision_id == row.revision_id
                && block.workspace_id == revision_scope.0
                && block.library_id == revision_scope.1
                && block.document_id == revision_scope.2
        }),
        "structured block payload does not match its authoritative revision scope"
    );

    let persisted_revision =
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_structured_revision (
                revision_id, workspace_id, library_id, document_id, preparation_state,
                normalization_profile, source_format, language_code, block_count, chunk_count,
                typed_fact_count, outline_json, prepared_at, updated_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
             ON CONFLICT (revision_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                library_id = EXCLUDED.library_id,
                document_id = EXCLUDED.document_id,
                preparation_state = EXCLUDED.preparation_state,
                normalization_profile = EXCLUDED.normalization_profile,
                source_format = EXCLUDED.source_format,
                language_code = EXCLUDED.language_code,
                block_count = EXCLUDED.block_count,
                chunk_count = EXCLUDED.chunk_count,
                typed_fact_count = EXCLUDED.typed_fact_count,
                outline_json = EXCLUDED.outline_json,
                prepared_at = EXCLUDED.prepared_at,
                updated_at = EXCLUDED.updated_at
             RETURNING {STRUCTURED_REVISION_COLUMNS}"
        )))
        .bind(row.revision_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(&row.preparation_state)
        .bind(&row.normalization_profile)
        .bind(&row.source_format)
        .bind(&row.language_code)
        .bind(i64::from(row.block_count))
        .bind(i64::from(row.chunk_count))
        .bind(i64::from(row.typed_fact_count))
        .bind(row.outline_json.clone())
        .bind(row.prepared_at)
        .bind(row.updated_at)
        .fetch_one(&mut **transaction)
        .await
        .context("failed to upsert knowledge structured revision")?;

    sqlx::query("DELETE FROM knowledge_structured_block WHERE revision_id = $1")
        .bind(row.revision_id)
        .execute(&mut **transaction)
        .await
        .context("failed to delete structured blocks during artifact replacement")?;

    const _: () = assert!(
        STRUCTURED_BLOCK_INSERT_BATCH_ROWS * STRUCTURED_BLOCK_INSERT_BIND_COUNT
            <= POSTGRES_MAX_BIND_PARAMETERS
    );
    for batch in blocks.chunks(STRUCTURED_BLOCK_INSERT_BATCH_ROWS) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "INSERT INTO knowledge_structured_block (
                block_id, workspace_id, library_id, document_id, revision_id, ordinal,
                block_kind, text, normalized_text, heading_trail, section_path, page_number,
                span_start, span_end, parent_block_id, table_coordinates_json, code_language,
                created_at, updated_at
             ) ",
        );
        builder.push_values(batch, |mut values, block| {
            values
                .push_bind(block.block_id)
                .push_bind(block.workspace_id)
                .push_bind(block.library_id)
                .push_bind(block.document_id)
                .push_bind(block.revision_id)
                .push_bind(block.ordinal)
                .push_bind(&block.block_kind)
                .push_bind(&block.text)
                .push_bind(&block.normalized_text)
                .push_bind(&block.heading_trail)
                .push_bind(&block.section_path)
                .push_bind(block.page_number)
                .push_bind(block.span_start)
                .push_bind(block.span_end)
                .push_bind(block.parent_block_id)
                .push_bind(&block.table_coordinates_json)
                .push_bind(&block.code_language)
                .push_bind(block.created_at)
                .push_bind(block.updated_at);
        });
        builder
            .build()
            .execute(&mut **transaction)
            .await
            .context("failed to insert structured block artifact batch")?;
    }
    Ok(persisted_revision)
}

pub(super) async fn replace_technical_facts_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    revision_id: Uuid,
    rows: &[KnowledgeTechnicalFactRow],
) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
    let revision_scope = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "SELECT workspace_id, library_id, document_id
         FROM knowledge_revision
         WHERE revision_id = $1
         FOR UPDATE",
    )
    .bind(revision_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to lock technical fact revision")?
    .context("technical fact revision does not exist")?;
    anyhow::ensure!(
        rows.iter().all(|row| {
            row.revision_id == revision_id
                && row.workspace_id == revision_scope.0
                && row.library_id == revision_scope.1
                && row.document_id == revision_scope.2
        }),
        "technical fact payload does not match its authoritative revision scope"
    );

    sqlx::query("DELETE FROM knowledge_technical_fact WHERE revision_id = $1")
        .bind(revision_id)
        .execute(&mut **transaction)
        .await
        .context("failed to delete technical facts during replacement")?;

    const _: () = assert!(
        TECHNICAL_FACT_INSERT_BATCH_ROWS * TECHNICAL_FACT_INSERT_BIND_COUNT
            <= POSTGRES_MAX_BIND_PARAMETERS
    );
    let mut inserted = Vec::with_capacity(rows.len());
    for batch in rows.chunks(TECHNICAL_FACT_INSERT_BATCH_ROWS) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "INSERT INTO knowledge_technical_fact (
                fact_id, workspace_id, library_id, document_id, revision_id, fact_kind,
                canonical_value_text, canonical_value_exact, canonical_value_json, display_value,
                qualifiers_json, support_block_ids, support_chunk_ids, confidence,
                extraction_kind, conflict_group_id, created_at, updated_at
             ) ",
        );
        builder.push_values(batch, |mut values, row| {
            values
                .push_bind(row.fact_id)
                .push_bind(row.workspace_id)
                .push_bind(row.library_id)
                .push_bind(row.document_id)
                .push_bind(row.revision_id)
                .push_bind(&row.fact_kind)
                .push_bind(&row.normalized_value_text)
                .push_bind(&row.normalized_value_exact)
                .push_bind(row.normalized_value_json.clone())
                .push_bind(&row.display_value)
                .push_bind(row.qualifiers_json.clone())
                .push_bind(&row.support_block_ids)
                .push_bind(&row.support_chunk_ids)
                .push_bind(row.confidence)
                .push_bind(&row.extraction_kind)
                .push_bind(&row.conflict_group_id)
                .push_bind(row.created_at)
                .push_bind(row.updated_at);
        });
        builder.push(format!(" RETURNING {TECHNICAL_FACT_COLUMNS}"));
        inserted.extend(
            builder
                .build_query_as::<KnowledgeTechnicalFactRow>()
                .fetch_all(&mut **transaction)
                .await
                .context("failed to insert technical fact replacement batch")?,
        );
    }
    Ok(inserted)
}

pub(super) fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

pub(super) fn active_chunks_by_library_page_sql() -> String {
    format!(
        "SELECT {QUALIFIED_CHUNK_COLUMNS}
         FROM knowledge_chunk c
         JOIN knowledge_document d
           ON d.document_id = c.document_id
          AND d.library_id = c.library_id
          AND d.readable_revision_id = c.revision_id
          AND d.document_state = 'active'
          AND d.deleted_at IS NULL
         JOIN knowledge_structured_revision r
           ON r.revision_id = c.revision_id
          AND r.workspace_id = c.workspace_id
          AND r.library_id = c.library_id
          AND r.document_id = c.document_id
          AND r.preparation_state = 'prepared'
         WHERE c.library_id = $1
           AND c.chunk_state = 'ready'
           AND (
             $2::integer IS NULL
             OR (c.chunk_index, c.chunk_id) > ($2::integer, $3::uuid)
           )
         ORDER BY c.chunk_index ASC, c.chunk_id ASC
         LIMIT $4"
    )
}

pub(super) fn normalized_search_terms(terms: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    terms
        .iter()
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            (!term.is_empty() && seen.insert(term.clone())).then_some(term)
        })
        .collect()
}

pub(super) fn normalized_revision_chunk_terms(terms: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut normalized = terms
        .iter()
        .filter_map(|term| {
            let value = term.trim().to_lowercase();
            if value.is_empty() {
                return None;
            }
            let value =
                value.chars().take(KNOWLEDGE_CHUNK_REVISION_TERM_MAX_CHARS).collect::<String>();
            seen.insert(value.clone()).then_some(value)
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|term| Reverse(term.chars().count()));
    normalized.truncate(KNOWLEDGE_CHUNK_REVISION_TERM_LIMIT);
    normalized
}

pub(super) fn normalized_windows(windows: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut windows = windows
        .iter()
        .filter_map(|(min_index, max_index)| {
            (max_index >= min_index).then_some((*min_index, *max_index))
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();

    let mut normalized = Vec::<(i32, i32)>::new();
    for (min_index, max_index) in windows {
        match normalized.last_mut() {
            Some((_, last_max)) if min_index <= last_max.saturating_add(1) => {
                *last_max = (*last_max).max(max_index);
            }
            _ => normalized.push((min_index, max_index)),
        }
    }
    normalized
}

pub(super) fn normalized_revision_windows(windows: &[(Uuid, i32, i32)]) -> Vec<(Uuid, i32, i32)> {
    windows
        .iter()
        .filter_map(|(revision_id, min_index, max_index)| {
            (max_index >= min_index).then_some((*revision_id, *min_index, *max_index))
        })
        .collect()
}

pub(super) fn quote_relation_name(relation_name: &str) -> String {
    relation_name.split('.').map(quote_ident).collect::<Vec<_>>().join(".")
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
