//! Primary HTTP surface for whole-workspace snapshot export and import.
//!
//! This is a materially different granularity than the per-library snapshot
//! in `content::snapshot` (that one archives a single library; this one
//! bundles every library owned by the workspace into a plain tar of
//! already-zstd-compressed per-library archives) — the DR/backup scenario
//! for an entire workspace, not a single library.
//!
//! Export: `GET /catalog/workspaces/{workspaceId}/snapshot?include=library_data,blobs`
//! streams a plain `application/x-tar` archive back to the caller, mirroring
//! `content::snapshot::export_library_snapshot`'s duplex-streaming shape.
//!
//! Import: `PUT /catalog/workspaces/{workspaceId}/snapshot?onConflict=reject|replace`
//! is a full-replace write (§2.2 — PUT is only ever a full replacement, never
//! a merge), so it uses `PUT` rather than the `POST` this endpoint used
//! before the redesign. The restore itself runs on a background task —
//! matching every other admitted-then-executed mutation in this codebase
//! (e.g. content revisions, library snapshot import) — and the request is
//! admitted with `202 Accepted` + `Location: /v1/ops/operations/{operationId}`
//! rather than blocking the caller until every embedded library is restored.

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::ASYNC_OP_STATUS_PROCESSING,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_WORKSPACE_ADMIN, POLICY_WORKSPACE_READ, load_workspace_and_authorize,
        },
        content::snapshot::{
            SnapshotBodySpool, authorize_snapshot_history_export,
            mark_snapshot_import_operation_failed, mark_snapshot_import_operation_ready,
            snapshot_result_payload, spool_snapshot_body,
        },
        router_support::ApiError,
    },
    services::content::service::snapshot::{
        IncludeKind, OverwriteMode, SnapshotImportError, WorkspaceSnapshotImportReport,
        export_workspace_archive, restore_workspace_archive,
    },
    services::ops::service::CreateAsyncOperationCommand,
};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ExportQuery {
    /// Comma-separated include kinds applied to every embedded library
    /// archive. Same vocabulary as the library snapshot export. Defaults to
    /// `library_data,blobs`.
    include: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ImportQuery {
    /// `reject` (default) or `replace`. Each newly minted library inside the
    /// workspace is always restored in `replace` mode — this selects whether
    /// the import as a whole rejects when any embedded library slug already
    /// exists in the target workspace.
    #[serde(rename = "onConflict")]
    on_conflict: Option<OverwriteMode>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshotImportAcceptedResponse {
    pub operation_id: Uuid,
    pub workspace_id: Uuid,
    pub on_conflict: OverwriteMode,
    pub archive_bytes: u64,
}

/// What a settled workspace-snapshot import wrote, published as the operation's
/// `resultJson`. The per-library breakdown is the same shape the library-scoped
/// import publishes, so one poller reads both.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshotImportReportResponse {
    pub workspace_id: Uuid,
    pub overwrite_mode: OverwriteMode,
    pub libraries_restored: u64,
    pub libraries: Vec<WorkspaceLibraryImportReportResponse>,
    pub blobs_verified: u64,
    pub unverifiable_blobs: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLibraryImportReportResponse {
    pub library_id: Uuid,
    pub slug: String,
    pub postgres_rows_by_table: BTreeMap<String, u64>,
    pub blobs_restored: u64,
    pub blobs_verified: u64,
    pub unverifiable_blobs: u64,
}

impl From<&WorkspaceSnapshotImportReport> for WorkspaceSnapshotImportReportResponse {
    fn from(report: &WorkspaceSnapshotImportReport) -> Self {
        Self {
            workspace_id: report.workspace_id,
            overwrite_mode: report.overwrite_mode,
            libraries_restored: report.libraries_restored,
            libraries: report
                .libraries
                .iter()
                .map(|library| WorkspaceLibraryImportReportResponse {
                    library_id: library.library_id,
                    slug: library.slug.clone(),
                    postgres_rows_by_table: library
                        .postgres_rows_by_table
                        .iter()
                        .cloned()
                        .collect(),
                    blobs_restored: library.blobs_restored,
                    blobs_verified: library.blobs_verified,
                    unverifiable_blobs: library.unverifiable_blobs,
                })
                .collect(),
            blobs_verified: report.blobs_verified,
            unverifiable_blobs: report.unverifiable_blobs,
        }
    }
}

fn spawn_workspace_snapshot_import_worker(
    state: AppState,
    operation_id: Uuid,
    workspace_id: Uuid,
    spooled: SnapshotBodySpool,
    overwrite: OverwriteMode,
) {
    tokio::spawn(
        async move {
            let archive_bytes = spooled.bytes_written;
            let result = Box::pin(async {
                let reader = spooled
                    .open()
                    .await
                    .map_err(|error| SnapshotImportError::new(anyhow::Error::new(error)))?;
                Box::pin(restore_workspace_archive(&state, workspace_id, reader, overwrite)).await
            })
            .await;

            match result {
                Ok(report) => {
                    tracing::info!(
                        workspace_id = %report.workspace_id,
                        operation_id = %operation_id,
                        archive_bytes,
                        libraries_restored = report.libraries_restored,
                        blobs_verified = report.blobs_verified,
                        unverifiable_blobs = report.unverifiable_blobs,
                        "workspace snapshot import restored from spooled request body",
                    );
                    if report.unverifiable_blobs > 0 {
                        tracing::warn!(
                            workspace_id = %report.workspace_id,
                            operation_id = %operation_id,
                            unverifiable_blobs = report.unverifiable_blobs,
                            blobs_verified = report.blobs_verified,
                            "workspace snapshot import accepted blobs whose bytes could not be proven against content_revision.checksum; \
                             this check only proves the archive is internally consistent, not that it is authentic, \
                             because substituting both a blob and its checksum would also pass it",
                        );
                    }
                    let payload = snapshot_result_payload(
                        &WorkspaceSnapshotImportReportResponse::from(&report),
                    );
                    mark_snapshot_import_operation_ready(&state, operation_id, payload).await;
                }
                Err(failure) => {
                    tracing::error!(
                        %operation_id,
                        %workspace_id,
                        failure_code = failure.failure_code(),
                        "workspace snapshot import worker failed",
                    );
                    mark_snapshot_import_operation_failed(
                        &state,
                        operation_id,
                        failure.failure_code(),
                    )
                    .await;
                }
            }
        }
        .instrument(tracing::info_span!(
            "snapshot.workspace_import.worker",
            %operation_id,
            %workspace_id,
        )),
    );
}

/// Streams a whole-workspace snapshot as a plain `application/x-tar` archive
/// that bundles every library in the workspace (each embedded library
/// archive is already zstd-compressed).
#[utoipa::path(
    get,
    path = "/v1/catalog/workspaces/{workspaceId}/snapshot",
    tag = "catalog",
    operation_id = "exportCatalogWorkspaceSnapshot",
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
    authorize_snapshot_history_export(&auth)?;
    let workspace =
        load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_READ).await?;

    let include = match query.include.as_deref() {
        None | Some("") => vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        Some(raw) => IncludeKind::parse_csv(raw)
            .map_err(|error| ApiError::BadRequest(format!("invalid include: {error}")))?,
    };

    // 64 KiB duplex: same sizing rationale as the library snapshot exporter
    // in content::snapshot — generous enough that zstd blocks sort across
    // reasonable chunk sizes without starving the exporter, small enough
    // that slow clients don't let the exporter run ahead.
    let (writer, reader) = tokio::io::duplex(64 * 1024);
    let exporter_state = state.clone();
    let ws_id = workspace.id;
    let include_clone = include;
    let join = tokio::spawn(async move {
        Box::pin(export_workspace_archive(exporter_state, ws_id, include_clone, writer)).await
    });
    let observer_ws_id = ws_id;
    tokio::spawn(async move {
        match join.await {
            Ok(Ok(())) => {}
            Ok(Err(_error)) => {
                tracing::error!(
                    workspace_id = %observer_ws_id,
                    "workspace snapshot export failed",
                );
            }
            Err(_join_error) => {
                tracing::error!(
                    workspace_id = %observer_ws_id,
                    "workspace snapshot export task panicked or was cancelled",
                );
            }
        }
    });

    let stream = tokio_util::io::ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let filename =
        format!("workspace-{}-{}.tar", workspace.slug, chrono::Utc::now().format("%Y%m%dT%H%M%S"));
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

/// Restores a whole workspace from a plain-tar snapshot body (full-replace
/// `PUT`), provisioning one fresh library per embedded archive. Admits the
/// restore for async processing and returns `202 Accepted` with
/// `Location: /v1/ops/operations/{operationId}` — the same async-admission
/// pattern used by `content::create_revision` and the library snapshot
/// import, rather than blocking the request on a potentially very large
/// multi-library restore.
#[tracing::instrument(
    level = "info",
    name = "http.import_workspace_snapshot",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
#[utoipa::path(
    put,
    path = "/v1/catalog/workspaces/{workspaceId}/snapshot",
    tag = "catalog",
    operation_id = "importCatalogWorkspaceSnapshot",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier"),
        ("onConflict" = Option<OverwriteMode>, Query, description = "'reject' (default) or 'replace'"),
    ),
    request_body(
        content_type = "application/x-tar",
        description = "Plain tar archive previously emitted by GET /v1/catalog/workspaces/{workspaceId}/snapshot",
    ),
    responses(
        (status = 202, description = "Workspace snapshot import accepted; poll /v1/ops/operations/{operationId} until it is terminal. A failed bundle deletes the libraries it created, and its `failureCode` names the reason (`snapshot_target_populated`, `snapshot_source_already_present`, `snapshot_unsupported_schema_version`, `snapshot_archive_invalid`, or `snapshot_import_failed`)", body = WorkspaceSnapshotImportAcceptedResponse),
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
) -> Result<Response, ApiError> {
    authorize_snapshot_history_export(&auth)?;
    let workspace =
        load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let overwrite = query.on_conflict.unwrap_or_default();

    let spooled = spool_snapshot_body(body, "workspace").await?;
    let archive_bytes = spooled.bytes_written;
    let operation = state
        .application_services
        .ops
        .create_async_operation(
            &state,
            CreateAsyncOperationCommand {
                workspace_id: workspace.id,
                library_id: None,
                operation_kind: "snapshot_import".to_string(),
                surface_kind: "rest".to_string(),
                requested_by_principal_id: Some(auth.principal_id),
                status: ASYNC_OP_STATUS_PROCESSING.to_string(),
                subject_kind: "workspace".to_string(),
                subject_id: Some(workspace.id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;

    spawn_workspace_snapshot_import_worker(state, operation.id, workspace.id, spooled, overwrite);

    let body = WorkspaceSnapshotImportAcceptedResponse {
        operation_id: operation.id,
        workspace_id: workspace.id,
        on_conflict: overwrite,
        archive_bytes,
    };
    let mut response = (StatusCode::ACCEPTED, Json(body)).into_response();
    let location = format!("/v1/ops/operations/{}", operation.id);
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

/// Workspace snapshot routes. Wired as a `Router` because the import route
/// disables the global body-size limit — the caller can stream a multi-GB
/// archive as the request body, and tar is self-validating.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/catalog/workspaces/{workspace_id}/snapshot",
        MethodRouter::new()
            .get(export_workspace_snapshot)
            .put(import_workspace_snapshot)
            .layer(DefaultBodyLimit::disable()),
    )
}
