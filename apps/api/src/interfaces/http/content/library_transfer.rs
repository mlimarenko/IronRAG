use axum::{
    Json,
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
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::content::{
        service::UploadInlineDocumentCommand, source_access::describe_content_source,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SourceDownloadQuery {
    pub revision_id: Option<Uuid>,
}

pub(super) async fn export_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;

    let summaries = state.canonical_services.content.list_documents(&state, library.id).await?;

    let mut export_docs = Vec::new();
    for summary in &summaries {
        if summary.document.document_state == "deleted" {
            continue;
        }
        let revision_id = summary.head.as_ref().and_then(|head| head.effective_revision_id());
        let Some(revision_id) = revision_id else {
            continue;
        };

        let arango_rev = state.arango_document_store.get_revision(revision_id).await.ok().flatten();
        let content = arango_rev.and_then(|revision| revision.normalized_text).unwrap_or_default();
        if content.is_empty() {
            continue;
        }

        let title = summary
            .active_revision
            .as_ref()
            .and_then(|revision| revision.title.clone())
            .unwrap_or_else(|| summary.document.external_key.clone());
        let source_uri =
            summary.active_revision.as_ref().and_then(|revision| revision.source_uri.clone());
        let mime_type = summary
            .active_revision
            .as_ref()
            .map(|revision| revision.mime_type.clone())
            .unwrap_or_default();

        export_docs.push(serde_json::json!({
            "title": title,
            "sourceUri": source_uri,
            "mimeType": mime_type,
            "content": content,
        }));
    }

    let export = serde_json::json!({
        "version": "1.0",
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "library": {
            "displayName": library.display_name,
            "description": library.description,
            "extractionPrompt": library.extraction_prompt,
        },
        "documentCount": export_docs.len(),
        "documents": export_docs,
    });

    let filename = format!("{}.json", library.slug);
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Json(export),
    ))
}

pub(super) async fn import_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let docs = payload
        .get("documents")
        .and_then(|value| value.as_array())
        .ok_or_else(|| ApiError::BadRequest("missing documents array".to_string()))?;

    let mut imported = 0;
    for doc in docs {
        let title =
            doc.get("title").and_then(|value| value.as_str()).unwrap_or("Imported document");
        let content = doc.get("content").and_then(|value| value.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }
        let mime_type =
            doc.get("mimeType").and_then(|value| value.as_str()).unwrap_or("text/plain");

        state
            .canonical_services
            .content
            .upload_inline_document(
                &state,
                UploadInlineDocumentCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    external_key: None,
                    idempotency_key: None,
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "rest-import".to_string(),
                    source_identity: None,
                    file_name: format!("{title}.txt"),
                    title: Some(title.to_string()),
                    mime_type: Some(mime_type.to_string()),
                    file_bytes: content.as_bytes().to_vec(),
                },
            )
            .await?;

        imported += 1;
    }

    Ok(Json(serde_json::json!({
        "importedDocuments": imported,
        "libraryId": library_id,
    })))
}

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
        return Err(ApiError::BadRequest("document has no downloadable source".to_string()));
    }

    let storage_key = revision
        .storage_key
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or(state
            .canonical_services
            .content
            .resolve_revision_storage_key(&state, revision.id)
            .await?)
        .ok_or_else(|| {
            ApiError::BadRequest("document has no stored source to download".to_string())
        })?;
    let disposition = format!("attachment; filename=\"{}\"", descriptor.file_name);

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
