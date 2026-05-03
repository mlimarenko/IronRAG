//! Per-document source download handler.
//!
//! Returns the original revision blob (or a presigned redirect when the
//! storage backend supports it) for a single document. This is separate
//! from the library snapshot surface and is scoped to one revision.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentDocumentHead, ContentDocumentSummary, ContentRevision, ContentSourceAccess,
        ContentSourceAccessKind,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_READ, load_content_document_and_authorize},
        router_support::ApiError,
    },
    services::content::source_access::describe_content_source,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SourceDownloadQuery {
    pub revision_id: Option<Uuid>,
}

#[tracing::instrument(
    level = "info",
    name = "http.download_document_source",
    skip_all,
    fields(document_id = %document_id)
)]
pub(super) async fn download_document_source(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<SourceDownloadQuery>,
) -> Result<Response, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let summary = state.canonical_services.content.get_document(&state, document_id).await?;
    let revision =
        resolve_source_download_revision(&state, document_id, &summary, query.revision_id).await?;
    let descriptor = describe_content_source(
        revision.document_id,
        Some(revision.id),
        &revision.content_source_kind,
        revision.source_uri.as_deref(),
        revision.storage_key.as_deref(),
        revision.title.as_deref(),
        &summary.document.external_key,
    );

    if let Some(ContentSourceAccess { kind: ContentSourceAccessKind::ExternalUrl, href }) =
        descriptor.access.as_ref()
    {
        return Ok(Redirect::temporary(href).into_response());
    }

    if descriptor.access.is_none() {
        if let Some(rendered_source) = state
            .canonical_services
            .content
            .render_revision_text_source(&state, revision.id)
            .await?
        {
            let disposition = format!("attachment; filename=\"{}\"", descriptor.file_name);
            return Ok((
                [
                    (header::CONTENT_TYPE, revision.mime_type),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                Body::from(rendered_source),
            )
                .into_response());
        }
        return Err(ApiError::BadRequest("document has no downloadable source".to_string()));
    }

    let storage_key =
        revision.storage_key.clone().filter(|value| !value.trim().is_empty()).or(state
            .canonical_services
            .content
            .resolve_revision_storage_key(&state, revision.id)
            .await?);
    let disposition = format!("attachment; filename=\"{}\"", descriptor.file_name);

    let Some(storage_key) = storage_key else {
        if let Some(rendered_source) = state
            .canonical_services
            .content
            .render_revision_text_source(&state, revision.id)
            .await?
        {
            return Ok((
                [
                    (header::CONTENT_TYPE, revision.mime_type),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                Body::from(rendered_source),
            )
                .into_response());
        }
        return Err(ApiError::BadRequest("document has no stored source to download".to_string()));
    };

    if let Some(href) = state
        .content_storage
        .resolve_download_redirect_url(&storage_key, &disposition, &revision.mime_type)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    {
        return Ok(Redirect::temporary(&href).into_response());
    }

    let bytes = state
        .content_storage
        .read_revision_source(&storage_key)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok((
        [(header::CONTENT_TYPE, revision.mime_type), (header::CONTENT_DISPOSITION, disposition)],
        Body::from(bytes),
    )
        .into_response())
}

async fn resolve_source_download_revision(
    state: &AppState,
    document_id: Uuid,
    summary: &ContentDocumentSummary,
    requested_revision_id: Option<Uuid>,
) -> Result<ContentRevision, ApiError> {
    let revision_id = requested_revision_id
        .or_else(|| summary.head.as_ref().and_then(ContentDocumentHead::effective_revision_id))
        .or_else(|| summary.active_revision.as_ref().map(|revision| revision.id))
        .ok_or_else(|| {
            ApiError::BadRequest(
                "document has no available revision source to download".to_string(),
            )
        })?;

    if let Some(active_revision) = summary.active_revision.as_ref()
        && active_revision.id == revision_id
    {
        return Ok(active_revision.clone());
    }

    state
        .canonical_services
        .content
        .list_revisions(state, document_id)
        .await?
        .into_iter()
        .find(|revision| revision.id == revision_id)
        .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))
}
