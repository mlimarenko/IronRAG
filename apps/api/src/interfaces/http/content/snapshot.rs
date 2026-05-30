//! Canonical HTTP surface for library snapshot export and import.
//!
//! Export: `GET /content/libraries/{id}/snapshot?include=library_data,blobs`
//! streams an `application/zstd` tar.zst archive back to the caller. The
//! writer runs in a background task, pushing bytes through a bounded
//! `tokio::io::duplex` channel whose read half is attached to the axum
//! response body. Back-pressure is natural — when the client stops
//! reading, the exporter task blocks on the next `append` call.
//!
//! Import: `POST /content/libraries/{id}/snapshot?overwrite=reject|replace`
//! takes the raw request body (no multipart) as an `AsyncRead` stream,
//! pipes it through a zstd decoder and a tar reader, and restores the
//! library. The `include` kinds are read from the archive's manifest —
//! the request does not carry them.

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::header,
    response::Response,
    routing::MethodRouter,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio_util::io::StreamReader;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE, load_library_and_authorize},
        router_support::ApiError,
    },
    services::content::service::snapshot::{
        IncludeKind, OverwriteMode, SnapshotImportReport, export_library_archive,
        restore_library_archive,
    },
};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ExportQuery {
    /// Comma-separated list of include kinds. Defaults to everything.
    include: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ImportQuery {
    /// `reject` (default) or `replace`.
    overwrite: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotImportReportResponse {
    pub library_id: Uuid,
    pub overwrite_mode: OverwriteMode,
    pub include_kinds: Vec<IncludeKind>,
    pub postgres_rows_by_table: BTreeMap<String, u64>,
    pub arango_docs_by_store: BTreeMap<String, u64>,
    pub arango_edges_by_store: BTreeMap<String, u64>,
    pub skipped_arango_edges_by_store: BTreeMap<String, u64>,
    pub blobs_restored: u64,
}

impl From<SnapshotImportReport> for SnapshotImportReportResponse {
    fn from(report: SnapshotImportReport) -> Self {
        Self {
            library_id: report.library_id,
            overwrite_mode: report.overwrite_mode,
            include_kinds: report.include_kinds,
            postgres_rows_by_table: report.postgres_rows_by_table.into_iter().collect(),
            arango_docs_by_store: report.arango_docs_by_collection.into_iter().collect(),
            arango_edges_by_store: report.arango_edges_by_collection.into_iter().collect(),
            skipped_arango_edges_by_store: report
                .skipped_arango_edges_by_collection
                .into_iter()
                .collect(),
            blobs_restored: report.blobs_restored,
        }
    }
}

/// Streams a library snapshot as `application/zstd` (tar.zst).
#[utoipa::path(
    get,
    path = "/v1/content/libraries/{libraryId}/snapshot",
    tag = "content",
    operation_id = "exportLibrarySnapshot",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Streaming tar.zst archive of the library", content_type = "application/zstd", body = String),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.export_library_snapshot",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn export_library_snapshot(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;

    let include = match query.include.as_deref() {
        None | Some("") => vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        Some(raw) => IncludeKind::parse_csv(raw)
            .map_err(|error| ApiError::BadRequest(format!("invalid include: {error}")))?,
    };

    // 64 KiB duplex: generous enough that zstd blocks sort across
    // reasonable chunk sizes without starving the exporter, small
    // enough that slow clients don't let the exporter run ahead.
    let (writer, reader) = tokio::io::duplex(64 * 1024);
    let exporter_state = state.clone();
    let lib_id = library.id;
    let include_clone = include.clone();
    // Wrap the export in a JoinHandle observer so a panic inside the
    // exporter does not silently terminate the writer half with the
    // client still receiving HTTP 200 on a half-written archive. On
    // the failure path the exporter itself appends an
    // EXPORT_FAILED.json sentinel tar entry before finalizing — this
    // observer is the second line of defense for genuine panics.
    let join = tokio::spawn(async move {
        export_library_archive(exporter_state, lib_id, include_clone, writer).await
    });
    let observer_lib_id = lib_id;
    tokio::spawn(async move {
        match join.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(
                    library_id = %observer_lib_id,
                    error = format!("{error:#}"),
                    "library snapshot export failed",
                );
            }
            Err(join_error) => {
                tracing::error!(
                    library_id = %observer_lib_id,
                    error = format!("{join_error}"),
                    "library snapshot export task panicked or was cancelled",
                );
            }
        }
    });

    let stream = tokio_util::io::ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let filename =
        format!("library-{}-{}.tar.zst", library.slug, chrono::Utc::now().format("%Y%m%dT%H%M%S"),);
    let disposition = format!("attachment; filename=\"{filename}\"");
    // Content-Encoding: identity opts out of the global CompressionLayer —
    // the body is already compressed (zstd) and double-compressing would
    // both waste CPU and mis-frame chunked responses on some browsers.
    let response = Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/zstd")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CONTENT_ENCODING, "identity")
        .body(body)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(response)
}

/// Restores a library from a tar.zst snapshot body.
#[tracing::instrument(
    level = "info",
    name = "http.import_library_snapshot",
    skip_all,
    fields(library_id = %library_id)
)]
#[utoipa::path(
    post,
    path = "/v1/content/libraries/{libraryId}/snapshot",
    tag = "content",
    operation_id = "importLibrarySnapshot",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        ("overwrite" = Option<String>, Query, description = "Overwrite mode: 'reject' (default) or 'replace'"),
    ),
    request_body(
        content_type = "application/zstd",
        description = "tar.zst archive previously emitted by GET /v1/content/libraries/{libraryId}/snapshot",
    ),
    responses(
        (status = 200, description = "Snapshot import report (per-table row counts, blob count, applied overwrite mode)", body = SnapshotImportReportResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
        (status = 409, description = "Library already populated and overwrite=reject"),
    ),
)]
pub async fn import_library_snapshot(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ImportQuery>,
    body: Body,
) -> Result<Json<SnapshotImportReportResponse>, ApiError> {
    load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let overwrite = OverwriteMode::parse(query.overwrite.as_deref().unwrap_or(""))
        .map_err(|error| ApiError::BadRequest(format!("invalid overwrite: {error}")))?;

    // Wrap the axum body stream into an AsyncRead for tokio-tar.
    let body_stream = body
        .into_data_stream()
        .map_err(|error| std::io::Error::other(format!("body stream error: {error}")));
    let reader = StreamReader::new(body_stream);

    let report = restore_library_archive(&state, library_id, reader, overwrite)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(report.into()))
}

/// Snapshot routes. Wired as a `Router` because the import route
/// disables the global body-size limit — the caller can stream a
/// multi-GB archive as the request body, and tar-zst is self-validating.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/content/libraries/{library_id}/snapshot",
        MethodRouter::new()
            .get(export_library_snapshot)
            .post(import_library_snapshot)
            .layer(DefaultBodyLimit::disable()),
    )
}
