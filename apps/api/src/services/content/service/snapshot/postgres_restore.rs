use super::*;

/// Recursively strips characters that `PostgreSQL` `text` and `jsonb` cannot
/// store from every `String` node in `value`.
///
/// `PostgreSQL` rejects the following when they appear inside a JSONB literal:
///
/// - U+0000 (null byte) — forbidden in `text`/`jsonb` by the SQL standard.
/// - Lone surrogate code points (U+D800–U+DFFF) — not valid Unicode scalar
///   values; `serde_json` can round-trip them as `\uD800`-style escapes but
///   `PostgreSQL`'s JSON parser treats them as an "unsupported Unicode escape
///   sequence" and aborts the statement.
///
/// All other Unicode scalar values (including multi-byte characters) are left
/// intact so that legitimate content in any language is fully preserved. This
/// function is a no-op on rows that contain no such characters.
pub(super) fn sanitize_json_for_postgres(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            // Fast path: skip the allocation when nothing needs removing.
            if s.chars().any(|c| c == '\u{0000}' || is_surrogate_char(c)) {
                *s = s.chars().filter(|c| *c != '\u{0000}' && !is_surrogate_char(*c)).collect();
            }
        }
        serde_json::Value::Array(arr) => {
            for element in arr.iter_mut() {
                sanitize_json_for_postgres(element);
            }
        }
        serde_json::Value::Object(map) => {
            for element in map.values_mut() {
                sanitize_json_for_postgres(element);
            }
        }
        _ => {}
    }
}

/// Returns `true` for code points in the surrogate range U+D800–U+DFFF.
///
/// Rust `char` only holds valid Unicode scalar values, so no char produced by
/// iterating a well-formed `&str` can ever be a surrogate. However
/// `serde_json` in some versions can round-trip lone-surrogate JSON escapes
/// (`\uD800`) into an internal byte representation that may survive as a
/// surrogate-range code point; comparing via `u32` is both safe and correct.
#[inline]
pub(super) fn is_surrogate_char(c: char) -> bool {
    (0xD800..=0xDFFF).contains(&(c as u32))
}

/// Bulk-insert up to `IMPORT_BATCH_ROWS` postgres rows in a single
/// statement. Uses `jsonb_populate_recordset` so every column of the
/// target table is reconstructed from the JSONB object keys.
pub(super) async fn insert_pg_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    mut rows: Vec<serde_json::Value>,
) -> anyhow::Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    validate_snapshot_pg_table_name(table)?;
    // Derived knowledge rows strip U+0000 and lone surrogates before JSONB.
    // Content identity/revision rows are immutable: never sanitize them into a
    // different semantic request; PostgreSQL refuses unrepresentable input.
    // Covers the derived paths:
    // jsonb_to_recordset, jsonb_populate_recordset, and the vector
    // jsonb_to_recordset inside insert_pg_vector_rows_bulk.
    if !POSTGRES_CONTENT_TABLES.contains(&table) {
        for row in &mut rows {
            sanitize_json_for_postgres(row);
        }
    }
    let count = rows.len();
    if is_runtime_vector_relation_name(table) {
        insert_pg_vector_rows_bulk(tx, table, rows).await?;
        return Ok(count as u64);
    }
    let table = require_known_snapshot_pg_table(table)?;
    let payload = serde_json::Value::Array(rows);
    if table == "catalog_library" {
        update_catalog_library_rows_from_snapshot(tx, &payload, count).await?;
        return Ok(count as u64);
    }
    let on_conflict = pg_insert_conflict_clause(table);
    let enforce_content_constraints = POSTGRES_CONTENT_TABLES.contains(&table);
    if enforce_content_constraints {
        sqlx::query("SET LOCAL session_replication_role = 'origin'")
            .execute(&mut **tx)
            .await
            .context("enable content lineage constraints")?;
    }
    let sql = if let Some(columns) = snapshot_pg_insert_columns(table) {
        format!(
            "INSERT INTO {table} ({columns})
             SELECT {columns}
             FROM jsonb_to_recordset($1) AS row({}){on_conflict}",
            snapshot_pg_recordset_columns(table)?
        )
    } else {
        format!(
            "INSERT INTO {table} SELECT * FROM jsonb_populate_recordset(null::{table}, $1){on_conflict}"
        )
    };
    let result = sqlx::query(sqlx::AssertSqlSafe(&*sql))
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .map_err(|error| classify_snapshot_insert_error(table, error))
        .with_context(|| format!("bulk insert {count} rows into {table}"))?;
    let written = result.rows_affected();
    if enforce_content_constraints {
        sqlx::query("SET LOCAL session_replication_role = 'replica'")
            .execute(&mut **tx)
            .await
            .context("resume projection snapshot import")?;
    }
    if written < count as u64
        && pg_insert_conflict_policy(table) == SnapshotInsertConflictPolicy::MustWriteEveryRow
    {
        let owners = describe_snapshot_row_collision(tx, table, &payload).await;
        return Err(snapshot_rows_lost_rejection(table, count as u64, written, owners.as_deref()));
    }
    Ok(written)
}

/// Names the libraries that already own the identities this section carries.
///
/// Only runs on the refusal path, so the extra round trip costs nothing in the
/// normal case. Failure to answer is not fatal: the refusal stands either way,
/// it just loses the "which library" detail.
pub(super) async fn describe_snapshot_row_collision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    payload: &serde_json::Value,
) -> Option<String> {
    let identity_column = snapshot_pg_identity_column(table)?;
    let sql = format!(
        "SELECT DISTINCT existing.library_id::text
         FROM {table} AS existing
         JOIN jsonb_to_recordset($1) AS incoming({identity_column} uuid)
           ON existing.{identity_column} = incoming.{identity_column}
         LIMIT 5"
    );
    let owners: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(&*sql))
        .bind(payload)
        .fetch_all(&mut **tx)
        .await
        .ok()?;
    if owners.is_empty() { None } else { Some(owners.join(", ")) }
}

/// Identity column a snapshot section is keyed by, for collision reporting.
pub(super) fn snapshot_pg_identity_column(table: &str) -> Option<&'static str> {
    match table {
        "content_document"
        | "content_mutation_reference"
        | "content_mutation"
        | "content_mutation_predecessor_audit" => Some("id"),
        "knowledge_document" => Some("document_id"),
        "knowledge_revision" | "knowledge_structured_revision" => Some("revision_id"),
        "knowledge_structured_block" => Some("block_id"),
        "knowledge_chunk" => Some("chunk_id"),
        "knowledge_entity" => Some("entity_id"),
        "knowledge_relation" => Some("relation_id"),
        "knowledge_evidence" => Some("evidence_id"),
        "knowledge_context_bundle" => Some("bundle_id"),
        "knowledge_technical_fact" => Some("fact_id"),
        _ => None,
    }
}

/// Turns a unique-violation into the classified refusal an operator can act on.
///
/// The content plane inserts without an `ON CONFLICT` target, so a snapshot
/// whose rows already live in this deployment aborts the statement with
/// SQLSTATE 23505. Left raw, that surfaces as `snapshot_import_failed` and an
/// operator has to read the server log to learn that the real cause is a
/// second copy of the same library.
pub(super) fn classify_snapshot_insert_error(table: &str, error: sqlx::Error) -> anyhow::Error {
    let code = error.as_database_error().and_then(sqlx::error::DatabaseError::code);
    if code.as_deref() == Some("23505") {
        return SnapshotImportRejection::new(
            SnapshotImportFailureKind::SourceAlreadyPresent,
            format!(
                "snapshot `{table}` identity or reservation namespace collides with existing rows; original identities and reservations cannot be adopted or rewritten"
            ),
        )
        .with_source(error)
        .into_error();
    }
    // SQLSTATE 23502: the archive omits a value a NOT NULL column requires —
    // for example an archive exported before retained `revision_sequence`
    // existed. The section inserts with `jsonb_populate_recordset`, which writes
    // SQL NULL for an absent key, so the statement aborts. Refuse it as a
    // malformed archive: never infer a default such as `readable`, which would
    // silently widen visibility for a document the archive actually suppressed.
    if code.as_deref() == Some("23502") {
        return SnapshotImportRejection::new(
            SnapshotImportFailureKind::ArchiveInvalid,
            format!(
                "the archive is missing a value the current schema requires for `{table}` (a NOT NULL column the archive does not carry). An archive written before this schema must be re-exported from a current deployment; it is refused atomically rather than restored with an inferred default."
            ),
        )
        .with_source(error)
        .into_error();
    }
    anyhow::Error::new(error)
}

/// Fails the restore when Postgres silently dropped rows the archive declared.
///
/// Knowledge-plane sections insert with `ON CONFLICT (<pk>) DO NOTHING` so that
/// a retried statement inside one restore is idempotent. That same clause,
/// however, swallows a collision with rows some OTHER library left behind — the
/// insert reports success, the section ends up empty, and the import finishes
/// `ready` on a library that can never answer a query. Compare what Postgres
/// wrote against what was handed to it and refuse instead.
pub(super) fn snapshot_rows_lost_rejection(
    table: &str,
    submitted: u64,
    written: u64,
    owners: Option<&str>,
) -> anyhow::Error {
    let owned_by = owners.map_or_else(String::new, |owners| {
        format!(" The colliding rows are owned by library {owners}.")
    });
    SnapshotImportRejection::new(
        SnapshotImportFailureKind::SourceAlreadyPresent,
        format!(
            "restoring `{table}` wrote {written} of {submitted} rows: the rest collided with rows already present in this deployment.{owned_by} A snapshot keeps its original identities, so it can only be restored into an empty library or back over the library it came from — restoring it alongside a live copy is refused rather than half-applied."
        ),
    )
    .into_error()
}

/// Whether a table's `ON CONFLICT` clause is a deliberate "keep what the target
/// already has" policy, or a replay guard that must never hide lost rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapshotInsertConflictPolicy {
    /// The target's own row wins by design — workspace identity and the
    /// portable AI configuration. An import must never clobber those, so a
    /// skipped row here is the intended outcome, not a lost one.
    AdoptExisting,
    /// Every submitted row must land. `DO NOTHING` on these tables exists only
    /// so a retry inside one restore is idempotent.
    MustWriteEveryRow,
}

pub(super) fn pg_insert_conflict_policy(table: &str) -> SnapshotInsertConflictPolicy {
    // A per-dim vector shard is written through a CTE whose `rows_affected`
    // reports the manifest upsert, not the vectors, and its `DISTINCT ON (key)`
    // can legitimately collapse rows — so the written-row count cannot be
    // compared there. The shard does not need its own guard: every vector hangs
    // off a `knowledge_chunk` row, that section is checked, and it is restored
    // from an earlier archive entry, so a collision fails the import before any
    // vector is reached.
    if is_runtime_vector_relation_name(table) {
        return SnapshotInsertConflictPolicy::AdoptExisting;
    }
    match table {
        "catalog_workspace"
        | "ai_provider_catalog"
        | "ai_model_catalog"
        | "ai_model_binding_purpose"
        | "ai_account"
        | "ai_binding" => SnapshotInsertConflictPolicy::AdoptExisting,
        _ => SnapshotInsertConflictPolicy::MustWriteEveryRow,
    }
}

pub(super) fn snapshot_pg_insert_columns(table: &str) -> Option<&'static str> {
    match table {
        "knowledge_document" => Some(
            "document_id, workspace_id, library_id, external_key, file_name, title, document_state, active_revision_id, readable_revision_id, latest_revision_no, parent_document_id, document_role, created_at, updated_at, deleted_at",
        ),
        "knowledge_revision" => Some(
            "revision_id, workspace_id, library_id, document_id, revision_number, revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type, checksum, title, byte_size, normalized_text, text_checksum, image_checksum, text_state, vector_state, graph_state, text_readable_at, vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at",
        ),
        "knowledge_structured_revision" => Some(
            "revision_id, workspace_id, library_id, document_id, preparation_state, normalization_profile, source_format, language_code, block_count, chunk_count, typed_fact_count, outline_json, prepared_at, updated_at",
        ),
        "knowledge_structured_block" => Some(
            "block_id, workspace_id, library_id, document_id, revision_id, ordinal, block_kind, text, normalized_text, heading_trail, section_path, page_number, span_start, span_end, parent_block_id, table_coordinates_json, code_language, occurred_at, occurred_until, created_at, updated_at",
        ),
        "knowledge_chunk" => Some(
            "chunk_id, workspace_id, library_id, document_id, revision_id, primary_block_id, chunk_index, chunk_kind, content_text, normalized_text, span_start, span_end, token_count, support_block_ids, section_path, heading_trail, literal_digest, chunk_state, text_generation, vector_generation, quality_score, window_text, occurred_at, occurred_until",
        ),
        "knowledge_technical_fact" => Some(
            "fact_id, workspace_id, library_id, document_id, revision_id, fact_kind, \u{63}anonical_value_text, \u{63}anonical_value_exact, \u{63}anonical_value_json, display_value, qualifiers_json, support_block_ids, support_chunk_ids, confidence, extraction_kind, conflict_group_id, created_at, updated_at",
        ),
        "knowledge_entity" => Some(
            "entity_id, workspace_id, library_id, \u{63}anonical_label, aliases, entity_type, entity_sub_type, summary, confidence, support_count, freshness_generation, entity_state, created_at, updated_at",
        ),
        "knowledge_entity_candidate" => Some(
            "candidate_id, workspace_id, library_id, revision_id, chunk_id, candidate_label, candidate_type, candidate_sub_type, normalization_key, confidence, extraction_method, candidate_state, created_at, updated_at",
        ),
        "knowledge_relation" => Some(
            "relation_id, workspace_id, library_id, subject_entity_id, object_entity_id, predicate, normalized_assertion, summary, confidence, support_count, contradiction_state, freshness_generation, relation_state, created_at, updated_at",
        ),
        "knowledge_relation_candidate" => Some(
            "candidate_id, workspace_id, library_id, revision_id, chunk_id, subject_label, subject_candidate_key, predicate, object_label, object_candidate_key, normalized_assertion, confidence, extraction_method, candidate_state, created_at, updated_at",
        ),
        "knowledge_evidence" => Some(
            "evidence_id, workspace_id, library_id, document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end, quote_text, literal_spans_json, summary, evidence_kind, extraction_method, confidence, evidence_state, freshness_generation, created_at, updated_at",
        ),
        "knowledge_context_bundle" => Some(
            "bundle_id, workspace_id, library_id, query_execution_id, bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids, verification_state, verification_warnings, freshness_snapshot, candidate_summary, assembly_diagnostics, created_at, updated_at",
        ),
        "knowledge_retrieval_trace" => Some(
            "trace_id, workspace_id, library_id, query_execution_id, bundle_id, trace_state, retrieval_strategy, candidate_counts, dropped_reasons, timing_breakdown, diagnostics_json, created_at, updated_at",
        ),
        "knowledge_bundle_chunk" => {
            Some("bundle_id, chunk_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_entity" => {
            Some("bundle_id, entity_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_relation" => {
            Some("bundle_id, relation_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_evidence" => {
            Some("bundle_id, evidence_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_chunk_entity_mention" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_evidence_entity_support" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_evidence_relation_support" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_vector_relation_manifest" => Some(
            "library_id, dim, vector_kind, embedding_model_key, relation_name, is_default, row_count, promoted, created_at",
        ),
        _ => None,
    }
}

pub(super) fn snapshot_pg_recordset_columns(table: &str) -> anyhow::Result<&'static str> {
    Ok(match table {
        "knowledge_document" => {
            "document_id uuid, workspace_id uuid, library_id uuid, external_key text, file_name text, title text, document_state text, active_revision_id uuid, readable_revision_id uuid, latest_revision_no bigint, parent_document_id uuid, document_role text, created_at timestamptz, updated_at timestamptz, deleted_at timestamptz"
        }
        "knowledge_revision" => {
            "revision_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_number bigint, revision_state text, revision_kind text, storage_ref text, source_uri text, document_hint text, mime_type text, checksum text, title text, byte_size bigint, normalized_text text, text_checksum text, image_checksum text, text_state text, vector_state text, graph_state text, text_readable_at timestamptz, vector_ready_at timestamptz, graph_ready_at timestamptz, superseded_by_revision_id uuid, created_at timestamptz"
        }
        "knowledge_structured_revision" => {
            "revision_id uuid, workspace_id uuid, library_id uuid, document_id uuid, preparation_state text, normalization_profile text, source_format text, language_code text, block_count bigint, chunk_count bigint, typed_fact_count bigint, outline_json jsonb, prepared_at timestamptz, updated_at timestamptz"
        }
        "knowledge_structured_block" => {
            "block_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, ordinal integer, block_kind text, text text, normalized_text text, heading_trail text[], section_path text[], page_number integer, span_start integer, span_end integer, parent_block_id uuid, table_coordinates_json jsonb, code_language text, occurred_at timestamptz, occurred_until timestamptz, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_chunk" => {
            "chunk_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, primary_block_id uuid, chunk_index integer, chunk_kind text, content_text text, normalized_text text, span_start integer, span_end integer, token_count integer, support_block_ids uuid[], section_path text[], heading_trail text[], literal_digest text, chunk_state text, text_generation bigint, vector_generation bigint, quality_score real, window_text text, occurred_at timestamptz, occurred_until timestamptz"
        }
        "knowledge_technical_fact" => {
            "fact_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, fact_kind text, \u{63}anonical_value_text text, \u{63}anonical_value_exact text, \u{63}anonical_value_json jsonb, display_value text, qualifiers_json jsonb, support_block_ids uuid[], support_chunk_ids uuid[], confidence double precision, extraction_kind text, conflict_group_id text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_entity" => {
            "entity_id uuid, workspace_id uuid, library_id uuid, \u{63}anonical_label text, aliases text[], entity_type text, entity_sub_type text, summary text, confidence double precision, support_count bigint, freshness_generation bigint, entity_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_entity_candidate" => {
            "candidate_id uuid, workspace_id uuid, library_id uuid, revision_id uuid, chunk_id uuid, candidate_label text, candidate_type text, candidate_sub_type text, normalization_key text, confidence double precision, extraction_method text, candidate_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_relation" => {
            "relation_id uuid, workspace_id uuid, library_id uuid, subject_entity_id uuid, object_entity_id uuid, predicate text, normalized_assertion text, summary text, confidence double precision, support_count bigint, contradiction_state text, freshness_generation bigint, relation_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_relation_candidate" => {
            "candidate_id uuid, workspace_id uuid, library_id uuid, revision_id uuid, chunk_id uuid, subject_label text, subject_candidate_key text, predicate text, object_label text, object_candidate_key text, normalized_assertion text, confidence double precision, extraction_method text, candidate_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence" => {
            "evidence_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, chunk_id uuid, block_id uuid, fact_id uuid, span_start integer, span_end integer, quote_text text, literal_spans_json jsonb, summary text, evidence_kind text, extraction_method text, confidence double precision, evidence_state text, freshness_generation bigint, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_context_bundle" => {
            "bundle_id uuid, workspace_id uuid, library_id uuid, query_execution_id uuid, bundle_state text, bundle_strategy text, requested_mode text, resolved_mode text, selected_fact_ids uuid[], verification_state text, verification_warnings jsonb, freshness_snapshot jsonb, candidate_summary jsonb, assembly_diagnostics jsonb, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_retrieval_trace" => {
            "trace_id uuid, workspace_id uuid, library_id uuid, query_execution_id uuid, bundle_id uuid, trace_state text, retrieval_strategy text, candidate_counts jsonb, dropped_reasons jsonb, timing_breakdown jsonb, diagnostics_json jsonb, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_bundle_chunk" => {
            "bundle_id uuid, chunk_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_entity" => {
            "bundle_id uuid, entity_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_relation" => {
            "bundle_id uuid, relation_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_evidence" => {
            "bundle_id uuid, evidence_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_chunk_entity_mention" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence_entity_support" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence_relation_support" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_vector_relation_manifest" => {
            "library_id uuid, dim integer, vector_kind text, embedding_model_key text, relation_name text, is_default boolean, row_count bigint, promoted boolean, created_at timestamptz"
        }
        other => bail!("no explicit recordset column list for `{other}`"),
    })
}

pub(super) fn pg_insert_conflict_clause(table: &str) -> &'static str {
    match table {
        // Workspace-scope rows can legitimately pre-exist on the target
        // stack. The local workspace row remains the source of truth.
        "catalog_workspace" => " ON CONFLICT DO NOTHING",
        // AI-config rows are keyed by stable ids (system catalogs) or
        // scope-partitioned natural keys (accounts/bindings, each with three
        // partial unique indexes per scope). A targetless `ON CONFLICT DO
        // NOTHING` swallows a collision on ANY of those unique constraints,
        // so whatever the target already holds wins — an import never
        // clobbers a deployment's existing AI configuration, and a
        // partial-key collision cannot abort the restore transaction.
        "ai_provider_catalog"
        | "ai_model_catalog"
        | "ai_model_binding_purpose"
        | "ai_account"
        | "ai_binding" => " ON CONFLICT DO NOTHING",
        "knowledge_document" => {
            " ON CONFLICT (document_id) DO UPDATE SET workspace_id = excluded.workspace_id, library_id = excluded.library_id, external_key = excluded.external_key, file_name = excluded.file_name, title = excluded.title, document_state = excluded.document_state, active_revision_id = excluded.active_revision_id, readable_revision_id = excluded.readable_revision_id, latest_revision_no = excluded.latest_revision_no, parent_document_id = excluded.parent_document_id, document_role = excluded.document_role, created_at = excluded.created_at, updated_at = excluded.updated_at, deleted_at = excluded.deleted_at"
        }
        "knowledge_revision" | "knowledge_structured_revision" => {
            " ON CONFLICT (revision_id) DO NOTHING"
        }
        "knowledge_structured_block" => " ON CONFLICT (block_id) DO NOTHING",
        "knowledge_chunk" => " ON CONFLICT (chunk_id) DO NOTHING",
        "knowledge_technical_fact" => " ON CONFLICT (fact_id) DO NOTHING",
        "knowledge_entity" => " ON CONFLICT (entity_id) DO NOTHING",
        "knowledge_entity_candidate" | "knowledge_relation_candidate" => {
            " ON CONFLICT (candidate_id) DO NOTHING"
        }
        "knowledge_relation" => " ON CONFLICT (relation_id) DO NOTHING",
        "knowledge_evidence" => " ON CONFLICT (evidence_id) DO NOTHING",
        "knowledge_context_bundle" => " ON CONFLICT (bundle_id) DO NOTHING",
        "knowledge_retrieval_trace" => " ON CONFLICT (trace_id) DO NOTHING",
        "knowledge_bundle_chunk" => {
            " ON CONFLICT (bundle_id, chunk_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_entity" => {
            " ON CONFLICT (bundle_id, entity_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_relation" => {
            " ON CONFLICT (bundle_id, relation_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_evidence" => {
            " ON CONFLICT (bundle_id, evidence_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_chunk_entity_mention"
        | "knowledge_evidence_entity_support"
        | "knowledge_evidence_relation_support" => {
            " ON CONFLICT (from_id, to_id, relation_type) DO UPDATE SET support = excluded.support, library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, updated_at = excluded.updated_at"
        }
        "knowledge_vector_relation_manifest" => {
            " ON CONFLICT (library_id, dim, vector_kind, embedding_model_key) DO UPDATE SET relation_name = excluded.relation_name, is_default = excluded.is_default, row_count = excluded.row_count, promoted = excluded.promoted"
        }
        _ => "",
    }
}

pub(super) async fn update_catalog_library_rows_from_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    payload: &serde_json::Value,
    count: usize,
) -> anyhow::Result<()> {
    let result = sqlx::query(catalog_library_snapshot_update_sql())
        .bind(payload)
        .execute(&mut **tx)
        .await
        .context("update catalog_library row from snapshot")?;
    if result.rows_affected() != count as u64 {
        bail!(
            "catalog_library snapshot expected to update {count} target rows but updated {}",
            result.rows_affected()
        );
    }
    Ok(())
}

pub(super) const fn catalog_library_snapshot_update_sql() -> &'static str {
    "UPDATE catalog_library AS target
         SET workspace_id = row.workspace_id,
             slug = row.slug,
             display_name = COALESCE(row.display_name, target.display_name),
             description = row.description,
             lifecycle_state = COALESCE(row.lifecycle_state, target.lifecycle_state),
             source_truth_version = COALESCE(row.source_truth_version, target.source_truth_version),
             extraction_prompt = row.extraction_prompt,
             ai_summary = row.ai_summary,
             created_by_principal_id = row.created_by_principal_id,
             created_at = COALESCE(row.created_at, target.created_at),
             updated_at = COALESCE(row.updated_at, target.updated_at),
             web_ingest_policy = COALESCE(row.web_ingest_policy, target.web_ingest_policy),
             chunking_template = COALESCE(row.chunking_template, target.chunking_template),
             recognition_policy = COALESCE(row.recognition_policy, target.recognition_policy),
             include_document_hint_in_mcp_answers = COALESCE(
                 row.include_document_hint_in_mcp_answers,
                 target.include_document_hint_in_mcp_answers
             ),
             retrieval_config = COALESCE(row.retrieval_config, target.retrieval_config)
         FROM jsonb_to_recordset($1) AS row(
             id uuid,
             workspace_id uuid,
             slug text,
             display_name text,
             description text,
             lifecycle_state catalog_library_lifecycle_state,
             source_truth_version bigint,
             extraction_prompt text,
             ai_summary text,
             created_by_principal_id uuid,
             created_at timestamptz,
             updated_at timestamptz,
             web_ingest_policy jsonb,
             chunking_template text,
             recognition_policy jsonb,
             include_document_hint_in_mcp_answers boolean,
             retrieval_config jsonb
         )
         WHERE target.id = row.id"
}
