//! Canonical library snapshot — streaming tar.zst export and import.
//!
//! The archive layout is:
//!
//! ```text
//! manifest.json                         # first — declares include kinds and table list
//! postgres/<table>/part-NNNNNN.ndjson   # chunked per table, 64 MiB cap per part
//! arango/<collection>/part-NNNNNN.ndjson
//! arango-edges/<collection>/part-NNNNNN.ndjson
//! blobs/<escaped-storage-key>           # raw bytes, one entry per content blob
//! summary.json                          # last — row counts observed during export
//! ```
//!
//! Export is a single tar stream wrapped in zstd. The `tokio_tar::Builder`
//! writes into a `ZstdEncoder` which writes into a `tokio::io::DuplexStream`
//! write half; the HTTP layer reads the other half as a response body
//! stream. Back-pressure is natural — if the client stops reading, the
//! exporter task blocks on the next `builder.append` and Postgres cursors
//! pause with it.
//!
//! Import takes the raw request body as an async stream, wraps it in a
//! zstd decoder, hands it to `tokio_tar::Archive`, and processes entries
//! in their serialized order. No temporary file is created — tar entries
//! are self-contained so the reader does not need seekable input.
//!
//! The `include` query parameter on export selects which families of
//! entities end up in the archive. Import does NOT take an include filter
//! — it trusts the manifest that the archive itself carries, which is the
//! canonical source of what was exported.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, anyhow, bail};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio_tar::{Archive, Builder, EntryType, Header};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::arangodb::{
        client::ArangoClient,
        collections::{
            KNOWLEDGE_BLOCK_CHUNK_EDGE, KNOWLEDGE_BUNDLE_CHUNK_EDGE, KNOWLEDGE_BUNDLE_ENTITY_EDGE,
            KNOWLEDGE_BUNDLE_EVIDENCE_EDGE, KNOWLEDGE_BUNDLE_RELATION_EDGE,
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION,
            KNOWLEDGE_EVIDENCE_SOURCE_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE, KNOWLEDGE_FACT_EVIDENCE_EDGE,
            KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_RELATION_OBJECT_EDGE,
            KNOWLEDGE_RELATION_SUBJECT_EDGE, KNOWLEDGE_REVISION_BLOCK_EDGE,
            KNOWLEDGE_REVISION_CHUNK_EDGE, KNOWLEDGE_REVISION_COLLECTION,
            KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION, KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
            KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
        },
    },
};

// ===========================================================================
// Public types
// ===========================================================================

/// Schema version of the snapshot archive format. Bumped any time the
/// manifest shape or on-disk layout changes in a backwards-incompatible
/// way.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 2;

/// Soft cap for a single NDJSON part inside the tar stream. Small enough
/// that no individual table part holds the entire table in memory, large
/// enough that tar header overhead stays negligible.
const CHUNK_BYTES_SOFT_CAP: usize = 64 * 1024 * 1024;

/// Hard cap on a single NDJSON row during import. Rows are read with
/// `read_until` against a bounded buffer; anything beyond this size
/// aborts the import. The biggest legitimate row in the current schema
/// is a `content_revision` with an embedded markdown blob; even very
/// verbose ones stay well under 16 MiB.
const MAX_IMPORT_LINE_BYTES: usize = 32 * 1024 * 1024;

/// Scope of a library snapshot.
///
/// A library is an atomic unit from the operator's point of view: its
/// documents, revisions, chunks, runtime graph, knowledge entities and
/// relations all describe the same thing and are worthless without
/// each other. The old enum splintered them across the internal
/// storage tiers (postgres / runtime_graph / arango / blobs) which
/// only leaked implementation detail into the UI — users had to
/// reason about "graph vs knowledge base vs content" even though
/// those share one mental model.
///
/// The canonical scope `LibraryData` therefore always includes every
/// non-blob row required to rebuild the library 1:1. `Blobs` is the
/// separate opt-in toggle for original source files (PDFs, images,
/// etc.); it is optional because a large library's source tree can
/// easily dwarf the rest of the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncludeKind {
    /// Everything owned by a library that is NOT a raw source file —
    /// postgres rows (content + runtime graph) and arango documents /
    /// edges (knowledge base).
    LibraryData,
    /// Original uploaded files (PDFs, docx, images, …) keyed by
    /// `content_revision.storage_key`.
    Blobs,
}

impl IncludeKind {
    pub fn parse_csv(input: &str) -> anyhow::Result<Vec<Self>> {
        let mut seen: HashSet<Self> = HashSet::new();
        let mut out: Vec<Self> = Vec::new();
        for raw in input.split(',') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let kind = match trimmed {
                "library_data" => Self::LibraryData,
                "blobs" => Self::Blobs,
                // Back-compat shims for archives written by the old
                // enum — they all collapse into `LibraryData`.
                "content" | "runtime_graph" | "knowledge" => Self::LibraryData,
                other => bail!("unknown include kind `{other}`"),
            };
            if seen.insert(kind) {
                out.push(kind);
            }
        }
        if out.is_empty() {
            bail!("`include` must name at least one kind");
        }
        Self::validate(&out)?;
        Ok(out)
    }

    /// Enforce dependency ordering. Blobs without LibraryData would
    /// produce orphan files with no `content_revision` row pointing
    /// at them — rejected.
    pub fn validate(kinds: &[Self]) -> anyhow::Result<()> {
        let has_library = kinds.contains(&Self::LibraryData);
        if kinds.contains(&Self::Blobs) && !has_library {
            bail!("include kind `blobs` requires `library_data`");
        }
        Ok(())
    }
}

/// Overwrite mode for restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverwriteMode {
    /// Fail the request if the library already exists (default).
    #[default]
    Reject,
    /// Delete all rows/documents/blobs under this library id, then
    /// insert everything from the archive. Not atomic across Postgres,
    /// Arango, and the blob store — a failed restore may leave the
    /// library in a partially-cleared state, and the same archive must
    /// be re-applied to converge.
    Replace,
}

impl OverwriteMode {
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        match input.trim() {
            "" | "reject" => Ok(Self::Reject),
            "replace" => Ok(Self::Replace),
            other => bail!("unknown overwrite mode `{other}`"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub library_id: Uuid,
    pub library_slug: String,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub source_version: String,
    pub include_kinds: Vec<IncludeKind>,
    pub postgres_tables: Vec<String>,
    pub arango_doc_collections: Vec<String>,
    pub arango_edge_collections: Vec<String>,
    pub has_blobs: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub postgres_row_counts: BTreeMap<String, u64>,
    pub arango_doc_counts: BTreeMap<String, u64>,
    pub arango_edge_counts: BTreeMap<String, u64>,
    pub blob_count: u64,
    pub missing_blob_keys: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SnapshotImportReport {
    pub library_id: Uuid,
    pub postgres_rows_by_table: Vec<(String, u64)>,
    pub arango_docs_by_collection: Vec<(String, u64)>,
    pub arango_edges_by_collection: Vec<(String, u64)>,
    pub blobs_restored: u64,
    pub overwrite_mode: OverwriteMode,
    pub include_kinds: Vec<IncludeKind>,
}

// ===========================================================================
// Section descriptors
// ===========================================================================

const POSTGRES_CONTENT_TABLES: &[&str] = &[
    "content_document",
    "content_revision",
    "content_chunk",
    "content_mutation",
    "content_mutation_item",
    "content_document_head",
];

const POSTGRES_RUNTIME_GRAPH_TABLES: &[&str] = &[
    "runtime_graph_snapshot",
    "runtime_graph_node",
    "runtime_graph_edge",
    "runtime_graph_evidence",
    "runtime_graph_canonical_summary",
];

const ARANGO_DOC_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_COLLECTION,
    KNOWLEDGE_REVISION_COLLECTION,
    KNOWLEDGE_CHUNK_COLLECTION,
    KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
    KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
    KNOWLEDGE_ENTITY_COLLECTION,
    KNOWLEDGE_RELATION_COLLECTION,
    KNOWLEDGE_EVIDENCE_COLLECTION,
];

const ARANGO_EDGE_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_REVISION_EDGE,
    KNOWLEDGE_REVISION_BLOCK_EDGE,
    KNOWLEDGE_REVISION_CHUNK_EDGE,
    KNOWLEDGE_BLOCK_CHUNK_EDGE,
    KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
    KNOWLEDGE_RELATION_SUBJECT_EDGE,
    KNOWLEDGE_RELATION_OBJECT_EDGE,
    KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
    KNOWLEDGE_FACT_EVIDENCE_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_CHUNK_EDGE,
    KNOWLEDGE_BUNDLE_ENTITY_EDGE,
    KNOWLEDGE_BUNDLE_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
];

// ===========================================================================
// Export
// ===========================================================================

/// Streams a tar.zst archive into `writer`. The writer is typically the
/// write half of a `tokio::io::duplex` whose read half is attached to an
/// axum response body, so the whole pipeline is back-pressure driven.
pub async fn export_library_archive<W>(
    state: AppState,
    library_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    IncludeKind::validate(&include)?;
    let include_set: HashSet<IncludeKind> = include.iter().copied().collect();

    let zstd = ZstdEncoder::new(writer);
    let mut builder = Builder::new(zstd);
    builder.mode(tokio_tar::HeaderMode::Deterministic);

    let pool = &state.persistence.postgres;
    let arango = state.arango_client.as_ref();

    // Resolve the library row first so we can fail fast and populate the
    // manifest's `library_slug` field.
    let library_row = sqlx::query("SELECT slug FROM catalog_library WHERE id = $1")
        .bind(library_id)
        .fetch_optional(pool)
        .await
        .context("load catalog_library slug")?
        .ok_or_else(|| anyhow!("library {library_id} does not exist"))?;
    let library_slug: String =
        library_row.try_get("slug").context("decode catalog_library slug")?;

    // Build the section plan from the include set. `LibraryData`
    // implies every content + runtime graph + knowledge table, which
    // is the only scope the UI ever exposes — storage-tier granular
    // flags leaked internal detail without helping the operator.
    let include_library_data = include_set.contains(&IncludeKind::LibraryData);
    let mut postgres_tables: Vec<String> = Vec::new();
    if include_library_data {
        postgres_tables.extend(POSTGRES_CONTENT_TABLES.iter().map(|s| (*s).to_string()));
        postgres_tables.extend(POSTGRES_RUNTIME_GRAPH_TABLES.iter().map(|s| (*s).to_string()));
    }
    let mut arango_docs: Vec<String> = Vec::new();
    let mut arango_edges: Vec<String> = Vec::new();
    if include_library_data {
        arango_docs.extend(ARANGO_DOC_COLLECTIONS.iter().map(|s| (*s).to_string()));
        arango_edges.extend(ARANGO_EDGE_COLLECTIONS.iter().map(|s| (*s).to_string()));
    }
    let has_blobs = include_set.contains(&IncludeKind::Blobs);

    // 1. manifest.json — first so readers can learn the shape immediately.
    let manifest = SnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        library_id,
        library_slug,
        exported_at: chrono::Utc::now(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        include_kinds: include.clone(),
        postgres_tables: postgres_tables.clone(),
        arango_doc_collections: arango_docs.clone(),
        arango_edge_collections: arango_edges.clone(),
        has_blobs,
    };
    append_json_entry(&mut builder, "manifest.json", &manifest).await?;

    // 2. postgres tables (content_document, content_revision, ...) — stream
    //    row-by-row via sqlx cursor, chunk into ~64 MiB parts, capture
    //    storage_key values along the way so we can export blobs later.
    let mut summary = SnapshotSummary::default();
    let mut storage_keys: HashSet<String> = HashSet::new();
    // catalog_library is exported implicitly as the very first pg entry
    // whenever the caller asked for library data, so a restore recreates
    // the row before any child table points at it.
    if include_library_data {
        let count = export_pg_catalog_library(&mut builder, pool, library_id).await?;
        summary.postgres_row_counts.insert("catalog_library".to_string(), count);
    }
    let pg_stage_started = std::time::Instant::now();
    for table in &postgres_tables {
        let table_started = std::time::Instant::now();
        let count = export_pg_table(
            &mut builder,
            pool,
            table,
            library_id,
            if table == "content_revision" { Some(&mut storage_keys) } else { None },
        )
        .await
        .with_context(|| format!("export postgres `{table}`"))?;
        summary.postgres_row_counts.insert(table.clone(), count);
        tracing::info!(
            %library_id,
            table = %table,
            rows = count,
            elapsed_ms = table_started.elapsed().as_millis() as u64,
            "snapshot export stage postgres",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = pg_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage postgres done",
    );

    // 3. arango doc collections
    let arango_doc_stage_started = std::time::Instant::now();
    for collection in &arango_docs {
        let col_started = std::time::Instant::now();
        let count = export_arango_doc_collection(&mut builder, arango, collection, library_id)
            .await
            .with_context(|| format!("export arango doc `{collection}`"))?;
        summary.arango_doc_counts.insert(collection.clone(), count);
        tracing::info!(
            %library_id,
            collection = %collection,
            rows = count,
            elapsed_ms = col_started.elapsed().as_millis() as u64,
            "snapshot export stage arango doc",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = arango_doc_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage arango docs done",
    );

    // 4. arango edge collections.
    //
    // Edges have no `library_id` column — they are filtered via their
    // endpoints. The DOCUMENT(edge._from) approach scans the full edge
    // collection which is slow on large shared databases, but passing
    // 400k+ vertex IDs as a bind-variable array is even worse (Arango
    // hash-join degrades on huge IN lists).
    //
    // Guard: if the doc-stage produced zero rows across ALL vertex
    // collections, edges are guaranteed empty too — skip the expensive
    // per-collection scans entirely.
    let arango_edge_stage_started = std::time::Instant::now();
    let has_any_arango_vertices = summary.arango_doc_counts.values().any(|count| *count > 0);
    if has_any_arango_vertices {
        for collection in &arango_edges {
            let col_started = std::time::Instant::now();
            let count = export_arango_edge_collection_via_document(
                &mut builder,
                arango,
                collection,
                library_id,
            )
            .await
            .with_context(|| format!("export arango edge `{collection}`"))?;
            summary.arango_edge_counts.insert(collection.clone(), count);
            tracing::info!(
                %library_id,
                collection = %collection,
                rows = count,
                elapsed_ms = col_started.elapsed().as_millis() as u64,
                "snapshot export stage arango edge",
            );
        }
    } else {
        for collection in &arango_edges {
            summary.arango_edge_counts.insert(collection.clone(), 0);
        }
        tracing::info!(
            %library_id,
            "snapshot export skipped arango edges — no matching vertices",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = arango_edge_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage arango edges done",
    );

    // 5. blobs (if included). Each storage_key gathered from the
    //    content_revision pass becomes one raw entry under `blobs/`.
    if has_blobs {
        for storage_key in &storage_keys {
            match state.content_storage.read_revision_source(storage_key).await {
                Ok(bytes) => {
                    append_raw_entry(
                        &mut builder,
                        &format!("blobs/{}", encode_blob_path(storage_key)),
                        &bytes,
                    )
                    .await
                    .with_context(|| format!("append blob {storage_key}"))?;
                    summary.blob_count += 1;
                }
                Err(error) => {
                    tracing::warn!(
                        %library_id,
                        storage_key = %storage_key,
                        error = format!("{error:#}"),
                        "snapshot skipping missing blob",
                    );
                    summary.missing_blob_keys.push(storage_key.clone());
                }
            }
        }
    }

    // 6. summary.json — last, so it carries the real observed counts.
    append_json_entry(&mut builder, "summary.json", &summary).await?;

    let zstd = builder.into_inner().await.context("finalize tar builder")?;
    let mut zstd = zstd;
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.context("finalize zstd stream")?;
    Ok(())
}

async fn append_json_entry<T, W>(
    builder: &mut Builder<W>,
    path: &str,
    value: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
    W: AsyncWrite + Unpin + Send,
{
    let bytes = serde_json::to_vec_pretty(value).context("serialize json entry")?;
    append_raw_entry(builder, path, &bytes).await
}

async fn append_raw_entry<W>(
    builder: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    // Use `append_data` instead of `append(&header, data)` so that
    // tokio-tar emits a GNU LongName extension header for paths that
    // exceed the 100-byte ustar limit. Blob storage keys routinely
    // reach ~160 chars (workspace + library + document + hash + ext).
    builder
        .append_data(&mut header, path, bytes)
        .await
        .with_context(|| format!("append tar entry `{path}`"))?;
    Ok(())
}

/// Escapes a storage key into a path-safe form that still round-trips.
/// Storage keys look like `content/<ws>/<lib>/<doc>/<hash>.bin` already,
/// so percent-encoding is overkill — but we still reject leading `/` and
/// parent traversal to keep the archive safe.
fn encode_blob_path(storage_key: &str) -> String {
    storage_key.trim_start_matches('/').replace("..", "__")
}

async fn export_pg_catalog_library<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send,
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
    append_raw_entry(builder, "postgres/catalog_library/part-000001.ndjson", &buffer).await?;
    Ok(1)
}

async fn export_pg_table<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    table: &str,
    library_id: Uuid,
    mut storage_keys: Option<&mut HashSet<String>>,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send,
{
    let query = build_pg_select(table);
    let mut stream = sqlx::query(&query).bind(library_id).fetch(pool);
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut row_count: u64 = 0;
    while let Some(row) = stream.next().await {
        let row = row.with_context(|| format!("stream {table}"))?;
        let value: serde_json::Value =
            row.try_get("row").with_context(|| format!("decode {table} row"))?;
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
            flush_pg_part(builder, table, &mut part_no, &mut buffer).await?;
        }
    }
    if !buffer.is_empty() {
        flush_pg_part(builder, table, &mut part_no, &mut buffer).await?;
    }
    Ok(row_count)
}

async fn flush_pg_part<W>(
    builder: &mut Builder<W>,
    table: &str,
    part_no: &mut u32,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    *part_no += 1;
    let path = format!("postgres/{table}/part-{part_no:06}.ndjson");
    append_raw_entry(builder, &path, buffer).await?;
    buffer.clear();
    Ok(())
}

fn build_pg_select(table: &str) -> String {
    match table {
        "content_chunk" => "SELECT row_to_json(c)::jsonb AS row
             FROM content_chunk c
             JOIN content_revision r ON r.id = c.revision_id
             WHERE r.library_id = $1
             ORDER BY c.id"
            .to_string(),
        "content_mutation_item" => "SELECT row_to_json(i)::jsonb AS row
             FROM content_mutation_item i
             JOIN content_mutation m ON m.id = i.mutation_id
             WHERE m.library_id = $1
             ORDER BY i.id"
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
        _ => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.id"
        ),
    }
}

/// Edge-collection export using the `library_id` field that every
/// edge now carries. Falls back to the slow DOCUMENT lookup on edges
/// that pre-date the migration (library_id == null), so old and new
/// data coexist cleanly without a forced backfill.
async fn export_arango_edge_collection_via_document<W>(
    builder: &mut Builder<W>,
    arango: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send,
{
    let query = "FOR edge IN @@collection
            FILTER edge.library_id == @library_id
            RETURN edge";
    let prefix = "arango-edges";
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<serde_json::Value>>(2);
    let bind_vars = serde_json::json!({
        "@collection": collection,
        "library_id": library_id.to_string(),
    });
    let query_owned = query.to_string();
    let arango_clone = arango.clone();
    let producer = tokio::spawn(async move {
        arango_clone
            .query_json_batches(&query_owned, bind_vars, |batch| {
                let tx = tx.clone();
                async move {
                    tx.send(batch).await.map_err(|_| anyhow!("arango stream receiver dropped"))?;
                    Ok(())
                }
            })
            .await
    });

    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut count: u64 = 0;
    while let Some(batch) = rx.recv().await {
        for row in batch {
            let mut line = serde_json::to_vec(&row)
                .with_context(|| format!("serialize {collection} edge to ndjson"))?;
            line.push(b'\n');
            buffer.extend_from_slice(&line);
            count += 1;
            if buffer.len() >= CHUNK_BYTES_SOFT_CAP {
                part_no += 1;
                let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
                append_raw_entry(builder, &path, &buffer).await?;
                buffer.clear();
            }
        }
    }
    if !buffer.is_empty() {
        part_no += 1;
        let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
        append_raw_entry(builder, &path, &buffer).await?;
    }
    producer
        .await
        .map_err(|error| anyhow!("arango producer join error: {error}"))?
        .with_context(|| format!("arango cursor {collection}"))?;
    Ok(count)
}

async fn export_arango_doc_collection<W>(
    builder: &mut Builder<W>,
    arango: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send,
{
    let query = "FOR doc IN @@collection FILTER doc.library_id == @library_id RETURN doc";
    let prefix = "arango";
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<serde_json::Value>>(2);
    let bind_vars = serde_json::json!({
        "@collection": collection,
        "library_id": library_id.to_string(),
    });
    let query_owned = query.to_string();
    let arango_clone = arango.clone();
    let producer = tokio::spawn(async move {
        arango_clone
            .query_json_batches(&query_owned, bind_vars, |batch| {
                let tx = tx.clone();
                async move {
                    tx.send(batch).await.map_err(|_| anyhow!("arango stream receiver dropped"))?;
                    Ok(())
                }
            })
            .await
    });

    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut count: u64 = 0;
    while let Some(batch) = rx.recv().await {
        for row in batch {
            let mut line = serde_json::to_vec(&row)
                .with_context(|| format!("serialize {collection} doc to ndjson"))?;
            line.push(b'\n');
            buffer.extend_from_slice(&line);
            count += 1;
            if buffer.len() >= CHUNK_BYTES_SOFT_CAP {
                part_no += 1;
                let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
                append_raw_entry(builder, &path, &buffer).await?;
                buffer.clear();
            }
        }
    }
    if !buffer.is_empty() {
        part_no += 1;
        let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
        append_raw_entry(builder, &path, &buffer).await?;
    }
    producer
        .await
        .map_err(|error| anyhow!("arango producer join error: {error}"))?
        .with_context(|| format!("arango cursor {collection}"))?;
    Ok(count)
}

// ===========================================================================
// Import
// ===========================================================================

/// Maximum number of rows included in a single Postgres or Arango
/// INSERT statement during restore. 1000 strikes a good balance: large
/// enough to amortize round-trip latency across a ten-thousand-row
/// table, small enough that a single statement's JSONB payload stays
/// under a few MiB and any parser bug only wastes a small slice.
const IMPORT_BATCH_ROWS: usize = 1000;

/// Restores a library from a tar.zst archive body. `body` is any
/// `AsyncRead` — typically the request body stream. Rows are flushed
/// to storage in batches as the archive streams in, so memory footprint
/// stays roughly one batch per backend (postgres/arango docs/arango edges)
/// rather than scaling with total archive size.
pub async fn restore_library_archive<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> anyhow::Result<SnapshotImportReport>
where
    R: AsyncRead + Unpin + Send,
{
    let decoder = ZstdDecoder::new(BufReader::new(body));
    let mut archive = Archive::new(decoder);
    let mut entries = archive.entries().context("open tar archive")?;

    let mut report =
        SnapshotImportReport { library_id, overwrite_mode: overwrite, ..Default::default() };
    let mut counts_pg: BTreeMap<String, u64> = BTreeMap::new();
    let mut counts_arango_doc: BTreeMap<String, u64> = BTreeMap::new();
    let mut counts_arango_edge: BTreeMap<String, u64> = BTreeMap::new();

    // Stage 1 — manifest must be the first tar entry. Any archive that
    // puts data ahead of it violates the snapshot protocol.
    let manifest = if let Some(entry) = entries.next().await {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;
        if path == "manifest.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read manifest.json")?;
            let parsed: SnapshotManifest =
                serde_json::from_slice(&bytes).context("parse manifest.json")?;
            if parsed.schema_version != SNAPSHOT_SCHEMA_VERSION {
                bail!(
                    "snapshot schema_version {} is not supported by this build (expected {})",
                    parsed.schema_version,
                    SNAPSHOT_SCHEMA_VERSION
                );
            }
            if parsed.library_id != library_id {
                bail!(
                    "snapshot library_id {} does not match target {library_id}",
                    parsed.library_id
                );
            }
            report.include_kinds = parsed.include_kinds.clone();
            parsed
        } else {
            bail!("tar entry `{path}` arrived before manifest.json");
        }
    } else {
        bail!("snapshot archive missing manifest.json");
    };

    // Stage 2 — pre-check the target library and apply the overwrite
    // policy BEFORE we start inserting. Replace-mode runs its own tx
    // because deletes and inserts do not need to be atomic across
    // phases; they only need to be individually correct so a retry can
    // converge.
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM catalog_library WHERE id = $1")
        .bind(library_id)
        .fetch_optional(&state.persistence.postgres)
        .await
        .context("pre-check catalog_library")?;
    match (exists.is_some(), overwrite) {
        (true, OverwriteMode::Reject) => {
            bail!(
                "library {library_id} already exists — pass overwrite=replace to restore over it"
            );
        }
        (true, OverwriteMode::Replace) => {
            clear_library_footprint(state, library_id, &manifest).await?;
        }
        (false, _) => {}
    }

    // Stage 3 — stream remaining entries and flush in batches. We keep
    // a single Postgres transaction alive for the whole restore so FKs
    // are satisfied all at once at commit time. For arango there is no
    // cross-collection transaction, so each batch stands on its own.
    let pool = &state.persistence.postgres;
    let mut tx = pool.begin().await.context("begin snapshot tx")?;
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut *tx)
        .await
        .context("disable FK checks for snapshot import")?;

    let arango = state.arango_client.as_ref();
    let mut pg_batcher = PgBatcher::new();
    let mut arango_doc_batcher = ArangoBatcher::new(false);
    let mut arango_edge_batcher = ArangoBatcher::new(true);

    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;

        if path == "summary.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read summary.json")?;
            if let Ok(parsed) = serde_json::from_slice::<SnapshotSummary>(&bytes) {
                tracing::info!(
                    %library_id,
                    declared_blob_count = parsed.blob_count,
                    declared_missing = parsed.missing_blob_keys.len(),
                    "snapshot summary read",
                );
            }
            continue;
        }

        if path == "manifest.json" {
            bail!("tar archive contains a second manifest.json");
        }

        if let Some(rest) = path.strip_prefix("postgres/") {
            let (table_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed postgres path `{path}`"))?;
            let table = table_ref.to_string();
            pg_batcher.on_new_section(&table, &mut tx).await?;
            read_ndjson_entry_and(&mut entry, &mut |row| {
                *counts_pg.entry(table.clone()).or_default() += 1;
                pg_batcher.push(&table, row);
                Ok(())
            })
            .await
            .with_context(|| format!("parse ndjson `{path}`"))?;
            pg_batcher.maybe_flush(&mut tx).await?;
        } else if let Some(rest) = path.strip_prefix("arango-edges/") {
            let (collection_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed arango-edges path `{path}`"))?;
            let collection = collection_ref.to_string();
            arango_edge_batcher.on_new_section(&collection, arango).await?;
            read_ndjson_entry_and(&mut entry, &mut |row| {
                *counts_arango_edge.entry(collection.clone()).or_default() += 1;
                arango_edge_batcher.push(&collection, row);
                Ok(())
            })
            .await?;
            arango_edge_batcher.maybe_flush(arango).await?;
        } else if let Some(rest) = path.strip_prefix("arango/") {
            let (collection_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed arango path `{path}`"))?;
            let collection = collection_ref.to_string();
            arango_doc_batcher.on_new_section(&collection, arango).await?;
            read_ndjson_entry_and(&mut entry, &mut |row| {
                *counts_arango_doc.entry(collection.clone()).or_default() += 1;
                arango_doc_batcher.push(&collection, row);
                Ok(())
            })
            .await?;
            arango_doc_batcher.maybe_flush(arango).await?;
        } else if let Some(blob_suffix) = path.strip_prefix("blobs/") {
            // Blobs are written as they arrive — they can be much larger
            // than a row so we never buffer them in a batcher.
            let storage_key = blob_suffix.to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read blob entry")?;
            state
                .content_storage
                .write_revision_source_raw(&storage_key, &bytes)
                .await
                .with_context(|| format!("write blob {storage_key}"))?;
            report.blobs_restored += 1;
        } else {
            bail!("unknown tar entry `{path}`");
        }
    }

    // Stage 4 — final flush + commit. Drain every batcher then commit
    // the Postgres transaction.
    pg_batcher.flush(&mut tx).await?;
    tx.commit().await.context("commit snapshot tx")?;
    arango_doc_batcher.flush(arango).await?;
    arango_edge_batcher.flush(arango).await?;

    report.postgres_rows_by_table = counts_pg.into_iter().collect();
    report.arango_docs_by_collection = counts_arango_doc.into_iter().collect();
    report.arango_edges_by_collection = counts_arango_edge.into_iter().collect();
    Ok(report)
}

fn validate_archive_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        bail!("tar entry with empty path");
    }
    if path.starts_with('/') {
        bail!("tar entry `{path}` is absolute");
    }
    for component in path.split('/') {
        if component == ".." {
            bail!("tar entry `{path}` contains parent traversal");
        }
    }
    Ok(())
}

fn split_section_path(rest: &str) -> anyhow::Result<(&str, &str)> {
    // Layout: <section>/<file>.ndjson
    rest.split_once('/').ok_or_else(|| anyhow!("path `{rest}` is not `<section>/<file>`"))
}

async fn read_ndjson_entry_and<R, F>(
    entry: &mut tokio_tar::Entry<R>,
    consume: &mut F,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(serde_json::Value) -> anyhow::Result<()>,
{
    let mut reader = BufReader::new(entry);
    let mut line: Vec<u8> = Vec::new();
    let mut line_no: usize = 0;
    loop {
        line.clear();
        let read = bounded_read_until(&mut reader, b'\n', &mut line, MAX_IMPORT_LINE_BYTES)
            .await
            .with_context(|| format!("read ndjson line {line_no}"))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        let trimmed = trim_trailing_newline(&line);
        if trimmed.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(trimmed)
            .with_context(|| format!("parse ndjson line {line_no}"))?;
        consume(value)?;
    }
    Ok(())
}

/// Buffers Postgres rows per-table and flushes them as a single
/// `jsonb_populate_recordset` statement. Each table keeps its own
/// pending vec; a section boundary or a full batch triggers a flush.
/// Replaces the row-by-row insert_pg_row path that was bottlenecked by
/// per-row round-trips on large libraries.
struct PgBatcher {
    current_table: Option<String>,
    pending: Vec<serde_json::Value>,
}

impl PgBatcher {
    fn new() -> Self {
        Self { current_table: None, pending: Vec::new() }
    }

    fn push(&mut self, table: &str, row: serde_json::Value) {
        // Only allocate a new String when the table changes. During a
        // 445 k-row structured_block restore this saves one String
        // clone per row — ~445 k allocs eliminated.
        if self.current_table.as_deref() != Some(table) {
            self.current_table = Some(table.to_string());
        }
        self.pending.push(row);
    }

    async fn on_new_section(
        &mut self,
        table: &str,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        if let Some(current) = self.current_table.as_deref()
            && current != table
        {
            self.flush(tx).await?;
        }
        Ok(())
    }

    async fn maybe_flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        while self.pending.len() >= IMPORT_BATCH_ROWS {
            self.flush_partial(tx, IMPORT_BATCH_ROWS).await?;
        }
        Ok(())
    }

    async fn flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        while !self.pending.is_empty() {
            let take = self.pending.len().min(IMPORT_BATCH_ROWS);
            self.flush_partial(tx, take).await?;
        }
        self.current_table = None;
        Ok(())
    }

    async fn flush_partial(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        take: usize,
    ) -> anyhow::Result<()> {
        let table = self
            .current_table
            .clone()
            .ok_or_else(|| anyhow!("flush_partial called with no current table"))?;
        let tail = self.pending.split_off(take.min(self.pending.len()));
        let head = std::mem::replace(&mut self.pending, tail);
        insert_pg_rows_bulk(tx, &table, head).await?;
        Ok(())
    }
}

/// Buffers Arango documents/edges for a single collection and flushes
/// them as a single AQL `FOR doc IN @docs INSERT` statement. Same
/// semantics as `PgBatcher` but keyed by collection instead of table.
struct ArangoBatcher {
    current_collection: Option<String>,
    pending: Vec<serde_json::Value>,
    is_edge: bool,
}

impl ArangoBatcher {
    fn new(is_edge: bool) -> Self {
        Self { current_collection: None, pending: Vec::new(), is_edge }
    }

    fn push(&mut self, collection: &str, row: serde_json::Value) {
        if self.current_collection.as_deref() != Some(collection) {
            self.current_collection = Some(collection.to_string());
        }
        self.pending.push(row);
    }

    async fn on_new_section(
        &mut self,
        collection: &str,
        arango: &ArangoClient,
    ) -> anyhow::Result<()> {
        if let Some(current) = self.current_collection.as_deref()
            && current != collection
        {
            self.flush(arango).await?;
        }
        Ok(())
    }

    async fn maybe_flush(&mut self, arango: &ArangoClient) -> anyhow::Result<()> {
        while self.pending.len() >= IMPORT_BATCH_ROWS {
            self.flush_partial(arango, IMPORT_BATCH_ROWS).await?;
        }
        Ok(())
    }

    async fn flush(&mut self, arango: &ArangoClient) -> anyhow::Result<()> {
        while !self.pending.is_empty() {
            let take = self.pending.len().min(IMPORT_BATCH_ROWS);
            self.flush_partial(arango, take).await?;
        }
        self.current_collection = None;
        Ok(())
    }

    async fn flush_partial(&mut self, arango: &ArangoClient, take: usize) -> anyhow::Result<()> {
        let collection = self
            .current_collection
            .clone()
            .ok_or_else(|| anyhow!("flush_partial called with no current collection"))?;
        let tail = self.pending.split_off(take.min(self.pending.len()));
        let head = std::mem::replace(&mut self.pending, tail);
        insert_arango_rows_bulk(arango, &collection, head, self.is_edge).await?;
        Ok(())
    }
}

async fn bounded_read_until<R>(
    reader: &mut BufReader<R>,
    delim: u8,
    buf: &mut Vec<u8>,
    max: usize,
) -> anyhow::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut total: usize = 0;
    loop {
        let available = reader.fill_buf().await.context("ndjson fill_buf")?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|b| *b == delim) {
            let slice = &available[..=pos];
            if total + slice.len() > max {
                bail!("ndjson line exceeds {max} bytes");
            }
            buf.extend_from_slice(slice);
            total += slice.len();
            let len = slice.len();
            reader.consume(len);
            return Ok(total);
        }
        let len = available.len();
        if total + len > max {
            bail!("ndjson line exceeds {max} bytes");
        }
        buf.extend_from_slice(available);
        total += len;
        reader.consume(len);
    }
}

fn trim_trailing_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

async fn clear_library_footprint(
    state: &AppState,
    library_id: Uuid,
    manifest: &SnapshotManifest,
) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;

    // Postgres: delete in reverse FK order so children disappear before
    // their parents. Run inside a single tx with FKs relaxed so that
    // CASCADE rules and stale references do not block the wipe.
    let mut tx = pool.begin().await.context("begin clear tx")?;
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut *tx)
        .await
        .context("disable FK checks for clear")?;

    // Tables to wipe for this library, in reverse dependency order.
    let mut reverse: Vec<&str> = Vec::new();
    for table in POSTGRES_RUNTIME_GRAPH_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_CONTENT_TABLES.iter().rev() {
        reverse.push(*table);
    }
    reverse.push("catalog_library");
    for table in reverse {
        let sql = match table {
            "content_chunk" => "DELETE FROM content_chunk c
                 USING content_revision r
                 WHERE r.id = c.revision_id AND r.library_id = $1"
                .to_string(),
            "content_mutation_item" => "DELETE FROM content_mutation_item i
                 USING content_mutation m
                 WHERE m.id = i.mutation_id AND m.library_id = $1"
                .to_string(),
            "content_document_head" => "DELETE FROM content_document_head h
                 USING content_document d
                 WHERE d.id = h.document_id AND d.library_id = $1"
                .to_string(),
            "catalog_library" => "DELETE FROM catalog_library WHERE id = $1".to_string(),
            _ => format!("DELETE FROM {table} WHERE library_id = $1"),
        };
        sqlx::query(&sql)
            .bind(library_id)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("clear pg table {table}"))?;
    }
    tx.commit().await.context("commit clear tx")?;

    // Arango: delete matching docs and edges. No cross-collection tx.
    let arango = state.arango_client.as_ref();
    for collection in ARANGO_EDGE_COLLECTIONS {
        arango
            .query_json_bulk(
                "FOR edge IN @@collection
                    FILTER edge.library_id == @library_id
                    REMOVE edge IN @@collection",
                serde_json::json!({
                    "@collection": *collection,
                    "library_id": library_id.to_string(),
                }),
            )
            .await
            .with_context(|| format!("clear arango edge {collection}"))?;
    }
    for collection in ARANGO_DOC_COLLECTIONS {
        arango
            .query_json_bulk(
                "FOR doc IN @@collection FILTER doc.library_id == @library_id REMOVE doc IN @@collection",
                serde_json::json!({
                    "@collection": *collection,
                    "library_id": library_id.to_string(),
                }),
            )
            .await
            .with_context(|| format!("clear arango doc {collection}"))?;
    }

    // Blobs: the storage backend owns a per-library directory. Stash it
    // (rename aside) before we insert new blobs so that a retry can
    // reuse the existing files if the manifest says `blobs` are not
    // part of the snapshot.
    if manifest.has_blobs
        && let Some(workspace_id) = load_library_workspace(pool, library_id).await?
    {
        let _ = state
            .content_storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .context("stash library blobs before restore")?;
    }

    Ok(())
}

async fn load_library_workspace(pool: &PgPool, library_id: Uuid) -> anyhow::Result<Option<Uuid>> {
    let row: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM catalog_library WHERE id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_library workspace for clear")?;
    Ok(row)
}

/// Bulk-insert up to `IMPORT_BATCH_ROWS` postgres rows in a single
/// statement. Uses `jsonb_populate_recordset` so every column of the
/// target table is reconstructed from the JSONB object keys.
async fn insert_pg_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    rows: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let count = rows.len();
    let sql =
        format!("INSERT INTO {table} SELECT * FROM jsonb_populate_recordset(null::{table}, $1)");
    let payload = serde_json::Value::Array(rows);
    sqlx::query(&sql)
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} rows into {table}"))?;
    Ok(())
}

/// Bulk-insert an Arango batch (documents or edges) as a single AQL
/// statement. Drops `_rev`/`_id` from each row before sending — they
/// are tied to the source deployment and are regenerated on insert.
async fn insert_arango_rows_bulk(
    arango: &ArangoClient,
    collection: &str,
    mut rows: Vec<serde_json::Value>,
    is_edge: bool,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    for row in &mut rows {
        if let Some(object) = row.as_object_mut() {
            object.remove("_rev");
            object.remove("_id");
        }
    }
    let count = rows.len();
    let doc_or_edge = if is_edge { "edge" } else { "doc" };
    arango
        .query_json_bulk(
            "FOR doc IN @docs INSERT doc INTO @@collection OPTIONS { overwriteMode: \"replace\" }",
            serde_json::json!({
                "@collection": collection,
                "docs": serde_json::Value::Array(rows),
            }),
        )
        .await
        .with_context(|| format!("bulk insert {count} {doc_or_edge}s into {collection}"))?;
    Ok(())
}
