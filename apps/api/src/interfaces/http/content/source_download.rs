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
use regex::{Captures, Regex};
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
        authorization::{POLICY_DOCUMENTS_READ, load_canonical_content_document_and_authorize},
        router_support::ApiError,
    },
    services::content::source_access::describe_content_source,
};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceDownloadQuery {
    pub revision_id: Option<Uuid>,
    pub representation: Option<SourceRepresentation>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceRepresentation {
    EditorMarkdown,
}

#[tracing::instrument(
    level = "info",
    name = "http.download_document_source",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}/source",
    tag = "content",
    operation_id = "getContentDocumentSource",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    responses(
        (status = 200, description = "Original document source bytes", content_type = "application/octet-stream", body = String),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document or revision not found"),
    ),
)]
pub async fn download_document_source(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<SourceDownloadQuery>,
) -> Result<Response, ApiError> {
    let _ = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_READ,
    )
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

    if query.representation == Some(SourceRepresentation::EditorMarkdown) {
        return download_editor_markdown_source(&state, &revision, &descriptor).await;
    }

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
        .map_err(ApiError::from)?
    {
        return Ok(Redirect::temporary(&href).into_response());
    }

    let bytes =
        state.content_storage.read_revision_source(&storage_key).await.map_err(ApiError::from)?;
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

async fn download_editor_markdown_source(
    state: &AppState,
    revision: &ContentRevision,
    descriptor: &crate::services::content::source_access::ContentSourceDescriptor,
) -> Result<Response, ApiError> {
    let rendered_source = match read_stored_editor_text_source(state, revision).await? {
        Some(stored_source) => Some(stored_source),
        None => {
            state.canonical_services.content.render_revision_text_source(state, revision.id).await?
        }
    }
    .map(|source| normalize_editor_markdown_source(revision, &source));

    let Some(rendered_source) = rendered_source else {
        return Err(ApiError::BadRequest("document has no rendered editor source".to_string()));
    };

    let disposition = format!("inline; filename=\"{}\"", descriptor.file_name);
    Ok((
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (header::CONTENT_DISPOSITION, disposition.as_str()),
        ],
        Body::from(rendered_source),
    )
        .into_response())
}

async fn read_stored_editor_text_source(
    state: &AppState,
    revision: &ContentRevision,
) -> Result<Option<String>, ApiError> {
    if !is_canonical_text_editor_source(&revision.mime_type) {
        return Ok(None);
    }
    let Some(storage_key) =
        revision.storage_key.as_deref().filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let bytes =
        state.content_storage.read_revision_source(storage_key).await.map_err(ApiError::from)?;
    String::from_utf8(bytes)
        .map(|text| Some(text.trim_end().to_string()))
        .map_err(|_| ApiError::BadRequest("stored source is not valid utf-8 text".to_string()))
}

fn is_canonical_text_editor_source(mime_type: &str) -> bool {
    let normalized =
        mime_type.split(';').next().map(str::trim).unwrap_or_default().to_ascii_lowercase();
    matches!(normalized.as_str(), "text/markdown" | "text/x-markdown")
}

fn normalize_editor_markdown_source(revision: &ContentRevision, source: &str) -> String {
    let markdown = source.trim_end().to_string();
    if revision.content_source_kind == "web_page" {
        resolve_markdown_image_urls(&markdown, revision.source_uri.as_deref())
    } else {
        markdown
    }
}

fn resolve_markdown_image_urls(markdown: &str, base_url: Option<&str>) -> String {
    let Some(base_url) = base_url.and_then(|value| reqwest::Url::parse(value).ok()) else {
        return markdown.to_string();
    };
    let Ok(image_destination) = Regex::new(r"(!\[[^\]\n]*\]\()([^\s)]+)(\))") else {
        return markdown.to_string();
    };

    image_destination
        .replace_all(markdown, |captures: &Captures<'_>| {
            let Some(destination) = captures.get(2).map(|item| item.as_str()) else {
                return captures[0].to_string();
            };
            let resolved = resolve_markdown_resource_url(&base_url, destination)
                .unwrap_or_else(|| destination.to_string());
            format!("{}{}{}", &captures[1], resolved, &captures[3])
        })
        .into_owned()
}

fn resolve_markdown_resource_url(base_url: &reqwest::Url, destination: &str) -> Option<String> {
    let trimmed = destination.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("data:")
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("tel:")
    {
        return None;
    }
    if let Some(protocol_relative) = trimmed.strip_prefix("//") {
        return Some(format!("{}://{}", base_url.scheme(), protocol_relative));
    }
    if reqwest::Url::parse(trimmed).is_ok() {
        return None;
    }
    base_url.join(trimmed).ok().map(|url| url.to_string())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::domains::content::ContentRevision;

    use super::{normalize_editor_markdown_source, resolve_markdown_image_urls};

    #[test]
    fn editor_markdown_resolves_relative_web_image_urls_against_source_uri() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "web_page".to_string(),
            checksum: "sha256:test".to_string(),
            mime_type: "text/html".to_string(),
            byte_size: 42,
            title: Some("Guide".to_string()),
            language_code: None,
            source_uri: Some("https://docs.example.test/space/page.html".to_string()),
            document_hint: None,
            storage_key: Some("content/snapshot".to_string()),
            created_by_principal_id: None,
            created_at: chrono::Utc::now(),
        };

        assert_eq!(
            normalize_editor_markdown_source(
                &revision,
                "Intro\n\n![Diagram](../images/flow.png)\n\n![Absolute](https://cdn.example.test/a.png)\n",
            ),
            "Intro\n\n![Diagram](https://docs.example.test/images/flow.png)\n\n![Absolute](https://cdn.example.test/a.png)"
        );
    }

    #[test]
    fn editor_markdown_keeps_non_web_image_destinations_unchanged() {
        assert_eq!(
            resolve_markdown_image_urls(
                "![Inline](data:image/png;base64,AA==)\n![Anchor](#diagram)",
                Some("https://docs.example.test/page"),
            ),
            "![Inline](data:image/png;base64,AA==)\n![Anchor](#diagram)"
        );
    }
}
