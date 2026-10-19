use super::*;

pub(super) async fn export_pg_catalog_library<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
    inventory_parts: &mut Vec<SnapshotEntryDigest>,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let row: serde_json::Value =
        sqlx::query_scalar("SELECT row_to_json(l)::jsonb FROM catalog_library l WHERE l.id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_library row")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during export"))?;
    let mut buffer = serde_json::to_vec(&row).context("serialize catalog_library row")?;
    buffer.push(b'\n');
    inventory_parts.push(
        append_raw_entry(builder, "postgres/catalog_library/part-000001.ndjson", &buffer).await?,
    );
    Ok(1)
}

/// Exports the workspace row that owns `library_id` plus the AI catalog
/// rows scoped to that workspace or library, so an import on a clean
/// stack satisfies `catalog_library.workspace_id` and recreates inherited
/// configured AI accounts and bindings in one shot.
///
/// Intentionally does NOT include `iam_api_token` / `iam_api_token_secret`
/// / `iam_principal` — those hashes are tied to a specific deployment
/// secret and must be re-issued on the target stack.
pub(super) async fn export_pg_workspace_scope<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
    inventory_parts: &mut Vec<SnapshotEntryDigest>,
) -> anyhow::Result<Vec<(String, u64)>>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let workspace_id: Uuid =
        sqlx::query_scalar("SELECT workspace_id FROM catalog_library WHERE id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load workspace id for library")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during export"))?;

    let mut counts = Vec::<(String, u64)>::new();

    // 1. catalog_workspace
    let ws_row: serde_json::Value =
        sqlx::query_scalar("SELECT row_to_json(w)::jsonb FROM catalog_workspace w WHERE w.id = $1")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_workspace row")?
            .ok_or_else(|| anyhow!("workspace {workspace_id} disappeared during export"))?;
    let mut buffer = serde_json::to_vec(&ws_row).context("serialize catalog_workspace row")?;
    buffer.push(b'\n');
    inventory_parts.push(
        append_raw_entry(builder, "postgres/catalog_workspace/part-000001.ndjson", &buffer).await?,
    );
    counts.push(("catalog_workspace".to_string(), 1));

    Ok(counts)
}

/// Exports the portable AI configuration that makes the exported library
/// resolvable on another stack: provider and model catalogs travel whole
/// (referenced by FK and seeded with stable ids on every deployment; each
/// model row carries its own pricing); accounts and bindings include
/// instance-scoped (deployment-global) rows plus the rows scoped to this
/// workspace/library. `api_key` is always nulled out —
/// provider secrets never leave the source stack. Bindings are filtered to
/// those whose account is also in the exported set so a restore never lands
/// a dangling FK.
pub(super) async fn export_pg_ai_config_scope<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
    inventory_parts: &mut Vec<SnapshotEntryDigest>,
) -> anyhow::Result<Vec<(String, u64)>>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let mut tx = pool.begin().await.context("begin ai-config snapshot export")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .context("set ai-config snapshot export isolation")?;
    let workspace_id: Uuid =
        sqlx::query_scalar("SELECT workspace_id FROM catalog_library WHERE id = $1")
            .bind(library_id)
            .fetch_optional(&mut *tx)
            .await
            .context("load workspace id for ai-config export")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during ai-config export"))?;

    // (table, query). Each query returns a single `row` jsonb column and is
    // bound with ($1 = workspace_id, $2 = library_id). The order matches the
    // FK-dependency order in POSTGRES_AI_CONFIG_TABLES.
    //
    // The scope filter keeps instance-scoped (deployment-global) rows AND the
    // rows scoped to this workspace/library. Instance scope is included on
    // purpose: in practice a deployment configures embed/answer bindings at
    // instance scope, so excluding them would make an AI-config export empty
    // for the common case. Import is non-destructive (ON CONFLICT DO NOTHING),
    // so importing instance config only ever populates an empty target.
    // `{alias}` is the table alias whose scope columns are tested. Instance
    // scope always matches; workspace/library scope matches this export's
    // ($1, $2). Phrased per-alias so the binding join can require its
    // account to be in the exported set too.
    let scope_pred = |alias: &str| {
        format!(
            "({alias}.scope_kind = 'instance' \
             OR ({alias}.scope_kind IN ('workspace','library') \
                 AND ({alias}.workspace_id = $1 OR {alias}.library_id = $2)))"
        )
    };
    let binding_query = format!(
        "SELECT row_to_json(b)::jsonb AS row FROM ai_binding b \
         JOIN ai_account a ON a.id = b.account_id AND {} \
         WHERE {}",
        scope_pred("a"),
        scope_pred("b"),
    );
    let account_query = format!(
        "SELECT (row_to_json(t)::jsonb || jsonb_build_object('api_key', NULL)) AS row \
         FROM ai_account t WHERE {}",
        scope_pred("t"),
    );
    let queries: [(&str, String); 5] = [
        (
            "ai_provider_catalog",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_provider_catalog t".to_string(),
        ),
        (
            "ai_model_catalog",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_model_catalog t".to_string(),
        ),
        (
            "ai_model_binding_purpose",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_model_binding_purpose t".to_string(),
        ),
        ("ai_account", account_query),
        ("ai_binding", binding_query),
    ];

    let mut exported_rows = Vec::with_capacity(queries.len());
    for (table, query) in &queries {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
            .bind(workspace_id)
            .bind(library_id)
            .fetch_all(&mut *tx)
            .await
            .with_context(|| format!("export ai-config table `{table}`"))?;
        exported_rows.push((*table, rows));
    }
    tx.commit().await.context("commit ai-config snapshot export")?;

    let mut counts = Vec::<(String, u64)>::with_capacity(exported_rows.len());
    for (table, rows) in exported_rows {
        counts.push((table.to_string(), rows.len() as u64));
        if rows.is_empty() {
            continue;
        }
        let mut buffer: Vec<u8> = Vec::new();
        for value in &rows {
            let mut line = serde_json::to_vec(value)
                .with_context(|| format!("serialize {table} ai-config row"))?;
            line.push(b'\n');
            buffer.extend_from_slice(&line);
        }
        let path = format!("postgres/{table}/part-000001.ndjson");
        inventory_parts.push(append_raw_entry(builder, &path, &buffer).await?);
    }

    Ok(counts)
}

pub(super) async fn export_pg_table<W>(
    ctx: &mut super::export::ExportStageContext<'_, W>,
    table: &str,
    mut storage_keys: Option<&mut HashSet<String>>,
    inventory_parts: &mut Vec<SnapshotEntryDigest>,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let select = build_pg_select(table)?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(&*select.sql)).bind(ctx.library_id);
    if select.binds_default_head_policy {
        // `$2` is the superseded sweep's head policy; the export keeps its
        // default, so a headless document's vectors stay in the archive.
        query = query.bind(false);
    }
    let mut stream = query.fetch(ctx.pool);
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut row_count: u64 = 0;
    while let Some(row) = stream.next().await {
        let row = row.with_context(|| format!("stream {table}"))?;
        let value: serde_json::Value =
            row.try_get("row").with_context(|| format!("decode {table} row"))?;
        ctx.content_closure.observe(table, &value)?;
        if let Some(keys) = storage_keys.as_deref_mut()
            && let Some(key) = value.get("storage_key").and_then(serde_json::Value::as_str)
            && !key.trim().is_empty()
        {
            keys.insert(key.to_string());
        }
        let mut line = serde_json::to_vec(&value)
            .with_context(|| format!("serialize {table} row to ndjson"))?;
        line.push(b'\n');
        buffer.extend_from_slice(&line);
        row_count += 1;
        if buffer.len() >= CHUNK_BYTES_SOFT_CAP {
            flush_pg_part(ctx.builder, table, &mut part_no, &mut buffer, inventory_parts).await?;
        }
    }
    if !buffer.is_empty() {
        flush_pg_part(ctx.builder, table, &mut part_no, &mut buffer, inventory_parts).await?;
    }
    Ok(row_count)
}

pub(super) async fn list_pg_vector_relations_for_library(
    pool: &PgPool,
    library_id: Uuid,
) -> anyhow::Result<Vec<String>> {
    let relations = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT relation_name
         FROM knowledge_vector_relation_manifest
         WHERE library_id = $1
         ORDER BY relation_name",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("list snapshot vector relations")?;
    relations
        .into_iter()
        .map(|relation| {
            validate_snapshot_pg_table_name(&relation)?;
            if !is_runtime_vector_relation_name(&relation) {
                bail!("snapshot vector manifest relation `{relation}` is not a vector relation");
            }
            Ok(relation)
        })
        .collect()
}

pub(super) async fn flush_pg_part<W>(
    builder: &mut Builder<W>,
    table: &str,
    part_no: &mut u32,
    buffer: &mut Vec<u8>,
    inventory_parts: &mut Vec<SnapshotEntryDigest>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    *part_no += 1;
    let path = format!("postgres/{table}/part-{part_no:06}.ndjson");
    inventory_parts.push(append_raw_entry(builder, &path, buffer).await?);
    buffer.clear();
    Ok(())
}

/// One table's export query and the binds it expects.
pub(super) struct PgSelect {
    pub(super) sql: String,
    /// The query also binds the superseded sweep's head policy as `$2`; the
    /// caller must bind `false` (the sweep's default) after the library id.
    pub(super) binds_default_head_policy: bool,
}

pub(super) fn build_pg_select(table: &str) -> anyhow::Result<PgSelect> {
    validate_snapshot_pg_table_name(table)?;
    if is_runtime_vector_relation_name(table) {
        let relation = quote_pg_identifier(table)?;
        let field_list = "jsonb_build_object(
                    'key', page.key,
                    'vector_id', page.vector_id,
                    'workspace_id', page.workspace_id,
                    'library_id', page.library_id,
                    'chunk_id', page.chunk_id,
                    'revision_id', page.revision_id,
                    'embedding_model_key', page.embedding_model_key,
                    'vector_kind', page.vector_kind,
                    'dimensions', page.dimensions,
                    'embedding', page.embedding::text,
                    'freshness_generation', page.freshness_generation,
                    'created_at', page.created_at,
                    'occurred_at', page.occurred_at,
                    'occurred_until', page.occurred_until
                 ) AS row";
        return Ok(if is_per_dim_chunk_vector_collection_name(table) {
            // The archive must not carry vectors the superseded sweep already
            // removed: restoring them would cancel the current-only lifecycle
            // (IRG-67). The predicate is the sweep's own, composed in one
            // place; the document join feeds it, the chunk join supplies the
            // ownership path.
            PgSelect {
                sql: format!(
                    "SELECT {field_list}
                     FROM {relation} page
                     LEFT JOIN knowledge_chunk chunk
                       ON chunk.library_id = page.library_id
                      AND chunk.chunk_id = page.chunk_id
                      AND chunk.revision_id = page.revision_id
                     LEFT JOIN knowledge_document document
                       ON document.library_id = page.library_id
                      AND document.document_id = chunk.document_id
                     WHERE page.library_id = $1
                       AND {}
                     ORDER BY page.key",
                    selectable_chunk_vector_rows_sql()
                ),
                binds_default_head_policy: true,
            }
        } else {
            PgSelect {
                sql: format!(
                    "SELECT jsonb_build_object(
                        'key', key,
                        'vector_id', vector_id,
                        'workspace_id', workspace_id,
                        'library_id', library_id,
                        'entity_id', entity_id,
                        'embedding_model_key', embedding_model_key,
                        'vector_kind', vector_kind,
                        'dimensions', dimensions,
                        'embedding', embedding::text,
                        'freshness_generation', freshness_generation,
                        'created_at', created_at
                     ) AS row
                     FROM {relation}
                     WHERE library_id = $1
                     ORDER BY key"
                ),
                binds_default_head_policy: false,
            }
        });
    }
    let table = require_known_snapshot_pg_table(table)?;
    let sql = match table {
        "content_chunk" => "SELECT row_to_json(c)::jsonb AS row
             FROM content_chunk c
             JOIN content_revision r ON r.id = c.revision_id
             WHERE r.library_id = $1
             ORDER BY c.id"
            .to_string(),
        "content_mutation_item" => "SELECT row_to_json(i)::jsonb AS row
             FROM content_mutation_item i
             JOIN content_mutation_reference m ON m.id = i.mutation_id
             WHERE m.library_id = $1
             ORDER BY i.id"
            .to_string(),
        "content_visibility_intent" => "SELECT row_to_json(v)::jsonb AS row
             FROM content_visibility_intent v
             JOIN content_document d ON d.id = v.document_id
             WHERE d.library_id = $1
             ORDER BY v.mutation_item_id"
            .to_string(),
        "content_document_head" => "SELECT row_to_json(h)::jsonb AS row
             FROM content_document_head h
             JOIN content_document d ON d.id = h.document_id
             WHERE d.library_id = $1
             ORDER BY h.document_id"
            .to_string(),
        "content_revision" => "SELECT row_to_json(t)::jsonb AS row
             FROM content_revision t
             WHERE t.library_id = $1
             ORDER BY t.document_id, t.revision_number"
            .to_string(),
        "runtime_graph_snapshot" => "SELECT row_to_json(t)::jsonb AS row
             FROM runtime_graph_snapshot t
             WHERE t.library_id = $1"
            .to_string(),
        "knowledge_vector_relation_manifest" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_vector_relation_manifest t
             WHERE t.library_id = $1
             ORDER BY t.dim, t.vector_kind, t.embedding_model_key"
            .to_string(),
        "knowledge_document" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_document t
             WHERE t.library_id = $1
             ORDER BY t.document_id"
            .to_string(),
        "knowledge_revision" | "knowledge_structured_revision" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.revision_id"
        ),
        "knowledge_retrieval_trace" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_retrieval_trace t
             WHERE t.library_id = $1
             ORDER BY t.trace_id"
            .to_string(),
        "knowledge_structured_block" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_structured_block t
             WHERE t.library_id = $1
             ORDER BY t.revision_id, t.ordinal, t.block_id"
            .to_string(),
        "knowledge_chunk" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_chunk t
             WHERE t.library_id = $1
             ORDER BY t.revision_id, t.chunk_index, t.chunk_id"
            .to_string(),
        "knowledge_technical_fact" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_technical_fact t
             WHERE t.library_id = $1
             ORDER BY t.fact_id"
            .to_string(),
        "knowledge_entity" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_entity t
             WHERE t.library_id = $1
             ORDER BY t.entity_id"
            .to_string(),
        "knowledge_entity_candidate" | "knowledge_relation_candidate" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.candidate_id"
        ),
        "knowledge_relation" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_relation t
             WHERE t.library_id = $1
             ORDER BY t.relation_id"
            .to_string(),
        "knowledge_evidence" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_evidence t
             WHERE t.library_id = $1
             ORDER BY t.evidence_id"
            .to_string(),
        "knowledge_context_bundle" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_context_bundle t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id"
            .to_string(),
        "knowledge_bundle_chunk" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_chunk t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.chunk_id"
            .to_string(),
        "knowledge_bundle_entity" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_entity t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.entity_id"
            .to_string(),
        "knowledge_bundle_relation" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_relation t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.relation_id"
            .to_string(),
        "knowledge_bundle_evidence" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_evidence t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.evidence_id"
            .to_string(),
        "knowledge_chunk_entity_mention"
        | "knowledge_evidence_entity_support"
        | "knowledge_evidence_relation_support" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.from_id, t.to_id, t.relation_type"
        ),
        _ => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.id"
        ),
    };
    Ok(PgSelect { sql, binds_default_head_policy: false })
}
