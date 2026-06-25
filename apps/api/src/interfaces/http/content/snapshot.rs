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
    http::{StatusCode, header},
    response::Response,
    routing::MethodRouter,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use tokio::io::AsyncWriteExt;
use tokio_util::io::StreamReader;
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{ASYNC_OP_STATUS_FAILED, ASYNC_OP_STATUS_PROCESSING, ASYNC_OP_STATUS_READY},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE, POLICY_WORKSPACE_ADMIN,
            POLICY_WORKSPACE_READ, load_library_and_authorize, load_workspace_and_authorize,
        },
        router_support::ApiError,
    },
    services::content::service::snapshot::{
        IncludeKind, OverwriteMode, SnapshotImportReport, WorkspaceSnapshotImportReport,
        export_library_archive, export_workspace_archive, restore_library_archive,
        restore_workspace_archive,
    },
    services::ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ExportQuery {
    /// Comma-separated include kinds. Valid values: `library_data`
    /// (content + runtime graph + knowledge tables), `blobs` (original
    /// source files, requires `library_data`), `workspace` (the owning
    /// `catalog_workspace` row) and `ai_config` (portable AI configuration:
    /// provider/model catalogs, prices, credentials without `api_key`,
    /// presets and bindings). Defaults to `library_data,blobs`.
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
    pub blobs_restored: u64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotImportAcceptedResponse {
    pub operation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub overwrite_mode: OverwriteMode,
    pub archive_bytes: u64,
}

impl From<SnapshotImportReport> for SnapshotImportReportResponse {
    fn from(report: SnapshotImportReport) -> Self {
        Self {
            library_id: report.library_id,
            overwrite_mode: report.overwrite_mode,
            include_kinds: report.include_kinds,
            postgres_rows_by_table: report.postgres_rows_by_table.into_iter().collect(),
            blobs_restored: report.blobs_restored,
        }
    }
}

/// One library's restore counts inside a [`WorkspaceSnapshotImportReportResponse`].
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLibraryImportReportResponse {
    pub library_id: Uuid,
    pub slug: String,
    pub postgres_rows_by_table: BTreeMap<String, u64>,
    pub blobs_restored: u64,
}

/// Report returned by `POST /v1/catalog/workspaces/{workspaceId}/snapshot`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshotImportReportResponse {
    pub workspace_id: Uuid,
    pub overwrite_mode: OverwriteMode,
    pub libraries_restored: u64,
    pub libraries: Vec<WorkspaceLibraryImportReportResponse>,
}

impl From<WorkspaceSnapshotImportReport> for WorkspaceSnapshotImportReportResponse {
    fn from(report: WorkspaceSnapshotImportReport) -> Self {
        Self {
            workspace_id: report.workspace_id,
            overwrite_mode: report.overwrite_mode,
            libraries_restored: report.libraries_restored,
            libraries: report
                .libraries
                .into_iter()
                .map(|library| WorkspaceLibraryImportReportResponse {
                    library_id: library.library_id,
                    slug: library.slug,
                    postgres_rows_by_table: library.postgres_rows_by_table.into_iter().collect(),
                    blobs_restored: library.blobs_restored,
                })
                .collect(),
        }
    }
}

struct SnapshotBodySpool {
    _temp_file: tempfile::NamedTempFile,
    path: PathBuf,
    bytes_written: u64,
}

impl SnapshotBodySpool {
    async fn open(&self) -> Result<tokio::fs::File, ApiError> {
        tokio::fs::File::open(&self.path)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "open spooled snapshot body"))
    }
}

async fn spool_snapshot_body(
    body: Body,
    snapshot_kind: &'static str,
) -> Result<SnapshotBodySpool, ApiError> {
    let temp_file = tempfile::Builder::new()
        .prefix("ironrag-snapshot-import-")
        .tempfile()
        .map_err(|error| ApiError::internal_with_log(error, "create snapshot import temp file"))?;
    let path = temp_file.path().to_path_buf();
    let writer = temp_file
        .reopen()
        .map_err(|error| ApiError::internal_with_log(error, "open snapshot import temp file"))?;
    let mut writer = tokio::fs::File::from_std(writer);

    let body_stream = body
        .into_data_stream()
        .map_err(|error| std::io::Error::other(format!("body stream error: {error}")));
    let mut reader = StreamReader::new(body_stream);
    let bytes_written = tokio::io::copy(&mut reader, &mut writer)
        .await
        .map_err(|error| ApiError::BadRequest(format!("failed to read snapshot body: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| ApiError::internal_with_log(error, "flush snapshot import temp file"))?;
    drop(writer);

    tracing::info!(
        snapshot_kind,
        archive_bytes = bytes_written,
        "snapshot import request body spooled",
    );

    Ok(SnapshotBodySpool { _temp_file: temp_file, path, bytes_written })
}

fn spawn_library_snapshot_import_worker(
    state: AppState,
    operation_id: Uuid,
    library_id: Uuid,
    spooled: SnapshotBodySpool,
    overwrite: OverwriteMode,
) {
    tokio::spawn(
        async move {
            let archive_bytes = spooled.bytes_written;
            let result = async {
                let reader = spooled.open().await?;
                restore_library_archive(&state, library_id, reader, overwrite)
                    .await
                    .map_err(ApiError::from)
            }
            .await;

            match result {
                Ok(report) => {
                    tracing::info!(
                        library_id = %report.library_id,
                        operation_id = %operation_id,
                        archive_bytes,
                        "snapshot import restored from spooled request body",
                    );
                    if let Err(error) = state
                        .canonical_services
                        .ops
                        .update_async_operation(
                            &state,
                            UpdateAsyncOperationCommand {
                                operation_id,
                                status: ASYNC_OP_STATUS_READY.to_string(),
                                completed_at: Some(chrono::Utc::now()),
                                failure_code: None,
                            },
                        )
                        .await
                    {
                        tracing::error!(
                            %operation_id,
                            %library_id,
                            error = ?error,
                            "failed to mark snapshot import operation ready",
                        );
                    }
                }
                Err(error) => {
                    tracing::error!(
                        %operation_id,
                        %library_id,
                        error = ?error,
                        "snapshot import worker failed",
                    );
                    if let Err(update_error) = state
                        .canonical_services
                        .ops
                        .update_async_operation(
                            &state,
                            UpdateAsyncOperationCommand {
                                operation_id,
                                status: ASYNC_OP_STATUS_FAILED.to_string(),
                                completed_at: Some(chrono::Utc::now()),
                                failure_code: Some("snapshot_import_failed".to_string()),
                            },
                        )
                        .await
                    {
                        tracing::error!(
                            %operation_id,
                            %library_id,
                            error = ?update_error,
                            "failed to mark snapshot import operation failed",
                        );
                    }
                }
            }
        }
        .instrument(tracing::info_span!(
            "snapshot.library_import.worker",
            %operation_id,
            %library_id,
        )),
    );
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
        (status = 202, description = "Snapshot import accepted; poll /v1/ops/operations/{operationId}", body = SnapshotImportAcceptedResponse),
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
) -> Result<(StatusCode, Json<SnapshotImportAcceptedResponse>), ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let overwrite = OverwriteMode::parse(query.overwrite.as_deref().unwrap_or(""))
        .map_err(|error| ApiError::BadRequest(format!("invalid overwrite: {error}")))?;

    let spooled = spool_snapshot_body(body, "library").await?;
    let archive_bytes = spooled.bytes_written;
    let operation = state
        .canonical_services
        .ops
        .create_async_operation(
            &state,
            CreateAsyncOperationCommand {
                workspace_id: library.workspace_id,
                library_id: Some(library.id),
                operation_kind: "snapshot_import".to_string(),
                surface_kind: "rest".to_string(),
                requested_by_principal_id: Some(auth.principal_id),
                status: ASYNC_OP_STATUS_PROCESSING.to_string(),
                subject_kind: "library".to_string(),
                subject_id: Some(library.id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;

    spawn_library_snapshot_import_worker(state, operation.id, library.id, spooled, overwrite);

    Ok((
        StatusCode::ACCEPTED,
        Json(SnapshotImportAcceptedResponse {
            operation_id: operation.id,
            workspace_id: library.workspace_id,
            library_id: library.id,
            overwrite_mode: overwrite,
            archive_bytes,
        }),
    ))
}

/// Streams a workspace snapshot as a plain `application/x-tar` archive that
/// bundles every library in the workspace (each embedded library archive is
/// already zstd-compressed).
#[utoipa::path(
    get,
    path = "/v1/catalog/workspaces/{workspaceId}/snapshot",
    tag = "content",
    operation_id = "exportWorkspaceSnapshot",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier"),
        ("include" = Option<String>, Query, description = "Comma-separated include kinds applied to every library archive (default `library_data,blobs`)"),
    ),
    responses(
        (status = 200, description = "Streaming plain tar bundling every library archive", content_type = "application/x-tar", body = String),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
        (status = 404, description = "Workspace not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.export_workspace_snapshot",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
pub async fn export_workspace_snapshot(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let workspace =
        load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_READ).await?;

    let include = match query.include.as_deref() {
        None | Some("") => vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        Some(raw) => IncludeKind::parse_csv(raw)
            .map_err(|error| ApiError::BadRequest(format!("invalid include: {error}")))?,
    };

    let (writer, reader) = tokio::io::duplex(64 * 1024);
    let exporter_state = state.clone();
    let ws_id = workspace.id;
    let include_clone = include.clone();
    let join = tokio::spawn(async move {
        export_workspace_archive(exporter_state, ws_id, include_clone, writer).await
    });
    let observer_ws_id = ws_id;
    tokio::spawn(async move {
        match join.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(
                    workspace_id = %observer_ws_id,
                    error = format!("{error:#}"),
                    "workspace snapshot export failed",
                );
            }
            Err(join_error) => {
                tracing::error!(
                    workspace_id = %observer_ws_id,
                    error = format!("{join_error}"),
                    "workspace snapshot export task panicked or was cancelled",
                );
            }
        }
    });

    let stream = tokio_util::io::ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let filename =
        format!("workspace-{}-{}.tar", workspace.slug, chrono::Utc::now().format("%Y%m%dT%H%M%S"),);
    let disposition = format!("attachment; filename=\"{filename}\"");
    // Plain (uncompressed) tar — the embedded library archives are already
    // zstd. Content-Encoding: identity opts out of the global CompressionLayer.
    let response = Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/x-tar")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CONTENT_ENCODING, "identity")
        .body(body)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(response)
}

/// Restores a workspace from a plain-tar snapshot body, provisioning one fresh
/// library per embedded archive.
#[tracing::instrument(
    level = "info",
    name = "http.import_workspace_snapshot",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
#[utoipa::path(
    post,
    path = "/v1/catalog/workspaces/{workspaceId}/snapshot",
    tag = "content",
    operation_id = "importWorkspaceSnapshot",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier"),
        ("overwrite" = Option<String>, Query, description = "Overwrite mode recorded in the report: 'reject' (default) or 'replace'. Each newly minted library is always restored in replace mode."),
    ),
    request_body(
        content_type = "application/x-tar",
        description = "Plain tar archive previously emitted by GET /v1/catalog/workspaces/{workspaceId}/snapshot",
    ),
    responses(
        (status = 200, description = "Workspace snapshot import report (per-library row counts)", body = WorkspaceSnapshotImportReportResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
        (status = 404, description = "Workspace not found"),
    ),
)]
pub async fn import_workspace_snapshot(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ImportQuery>,
    body: Body,
) -> Result<Json<WorkspaceSnapshotImportReportResponse>, ApiError> {
    load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let overwrite = OverwriteMode::parse(query.overwrite.as_deref().unwrap_or(""))
        .map_err(|error| ApiError::BadRequest(format!("invalid overwrite: {error}")))?;

    let spooled = spool_snapshot_body(body, "workspace").await?;
    let reader = spooled.open().await?;

    let report = restore_workspace_archive(&state, workspace_id, reader, overwrite)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        workspace_id = %report.workspace_id,
        archive_bytes = spooled.bytes_written,
        libraries_restored = report.libraries_restored,
        "workspace snapshot import restored from spooled request body",
    );

    Ok(Json(report.into()))
}

/// Snapshot routes. Wired as a `Router` because the import routes
/// disable the global body-size limit — the caller can stream a
/// multi-GB archive as the request body, and tar(-zst) is self-validating.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/content/libraries/{library_id}/snapshot",
            MethodRouter::new()
                .get(export_library_snapshot)
                .post(import_library_snapshot)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/catalog/workspaces/{workspace_id}/snapshot",
            MethodRouter::new()
                .get(export_workspace_snapshot)
                .post(import_workspace_snapshot)
                .layer(DefaultBodyLimit::disable()),
        )
}
