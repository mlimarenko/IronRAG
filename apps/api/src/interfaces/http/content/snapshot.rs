//! Canonical HTTP surface for library snapshot export and import.
//!
//! Export: `GET /content/libraries/{id}/snapshot?include=content,runtime_graph,knowledge,blobs`
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
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
pub(super) struct ExportQuery {
    /// Comma-separated list of include kinds. Defaults to everything.
    include: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ImportQuery {
    /// `reject` (default) or `replace`.
    overwrite: Option<String>,
}

/// Streams a library snapshot as `application/zstd` (tar.zst).
#[tracing::instrument(
    level = "info",
    name = "http.export_library_snapshot",
    skip_all,
    fields(library_id = %library_id)
)]
pub(super) async fn export_library_snapshot(
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
    tokio::spawn(async move {
        if let Err(error) =
            export_library_archive(exporter_state, lib_id, include_clone, writer).await
        {
            tracing::error!(
                library_id = %lib_id,
                error = format!("{error:#}"),
                "library snapshot export failed",
            );
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
pub(super) async fn import_library_snapshot(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ImportQuery>,
    body: Body,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_WRITE)?;

    let overwrite = OverwriteMode::parse(query.overwrite.as_deref().unwrap_or(""))
        .map_err(|error| ApiError::BadRequest(format!("invalid overwrite: {error}")))?;

    // Wrap the axum body stream into an AsyncRead for tokio-tar.
    let body_stream = body
        .into_data_stream()
        .map_err(|error| std::io::Error::other(format!("body stream error: {error}")));
    let reader = StreamReader::new(body_stream);

    let report = restore_library_archive(&state, library_id, reader, overwrite).await.map_err(
        |error: anyhow::Error| {
            let message = error.to_string();
            if message.contains("already exists") {
                ApiError::Conflict(message)
            } else {
                ApiError::BadRequest(message)
            }
        },
    )?;

    Ok(Json(import_report_to_json(&report)))
}

fn import_report_to_json(report: &SnapshotImportReport) -> serde_json::Value {
    serde_json::json!({
        "libraryId": report.library_id,
        "overwriteMode": report.overwrite_mode,
        "includeKinds": report.include_kinds,
        "postgresRowsByTable": report.postgres_rows_by_table.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "arangoDocsByCollection": report.arango_docs_by_collection.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "arangoEdgesByCollection": report.arango_edges_by_collection.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "blobsRestored": report.blobs_restored,
    })
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
