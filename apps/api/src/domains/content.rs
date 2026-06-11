use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::knowledge::StructuredDocumentRevision;
use ironrag_contracts::documents::DocumentReadiness;

pub use crate::domains::runtime_ingestion::RuntimeDocumentActivityStatus;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentDocument {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentDocumentHead {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
    pub head_updated_at: DateTime<Utc>,
    pub document_summary: Option<String>,
}

impl ContentDocumentHead {
    /// Returns the best revision available for serving content: prefers the last
    /// successfully-ingested (`readable`) revision, falls back to `active` if no
    /// readable revision exists yet.
    #[must_use]
    pub fn effective_revision_id(&self) -> Option<Uuid> {
        self.readable_revision_id.or(self.active_revision_id)
    }

    /// Returns the most recent revision pointer (active first, then readable)
    /// for use as the base revision when creating new mutations.
    #[must_use]
    pub fn latest_revision_id(&self) -> Option<Uuid> {
        self.active_revision_id.or(self.readable_revision_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentRevision {
    pub id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentSourceAccessKind {
    StoredDocument,
    ExternalUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentSourceAccess {
    pub kind: ContentSourceAccessKind,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentRevisionReadiness {
    pub revision_id: Uuid,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentMutation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub mutation_state: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentChunk {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: String,
    pub text_checksum: String,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; None for non-temporal sources like PDF/image/markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentMutationItem {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

pub const READABLE_TEXT_STATES: &[&str] = &["readable", "ready", "text_readable"];

#[must_use]
pub fn revision_text_state_is_readable(text_state: &str) -> bool {
    READABLE_TEXT_STATES.contains(&text_state.trim())
}

/// A document that carries its own evidence and competes as a peer in
/// retrieval. The default for every newly admitted document.
pub const DOCUMENT_ROLE_PRIMARY: &str = "primary";
/// A child of another document whose media class is non-image (e.g. a PDF
/// manual attached to a page). It keeps peer-document recall.
pub const DOCUMENT_ROLE_ATTACHMENT: &str = "attachment";
/// A child of another document whose media class is a raster image. It is
/// subordinate context of its parent rather than a competing peer document.
pub const DOCUMENT_ROLE_ATTACHED_CONTEXT: &str = "attached_context";

/// Whether a document's role makes its chunks subordinate context of a parent
/// document rather than independent peers competing in retrieval. Retrieval
/// surfaces read ONLY this typed role — never a MIME, extension, or filename
/// signal — to decide demotion.
#[must_use]
pub fn role_is_attached_context(role: &str) -> bool {
    role == DOCUMENT_ROLE_ATTACHED_CONTEXT
}

/// The document identity a retrieval surface should group and cap by. An
/// `attached_context` document collapses onto its parent (when resolved) so its
/// chunks share the parent's per-document slot instead of flooding as N
/// independent siblings; every other role keeps its own id.
#[must_use]
pub fn effective_document_id(
    role: &str,
    parent_document_id: Option<uuid::Uuid>,
    own_id: uuid::Uuid,
) -> uuid::Uuid {
    if role_is_attached_context(role) { parent_document_id.unwrap_or(own_id) } else { own_id }
}

/// Canonical, language-/extension-agnostic mapping from structural inputs to
/// the typed `content_document.document_role`. The single source of truth for
/// role derivation: every admission, promote, and backfill site routes through
/// this function so the decision is identical everywhere.
///
/// - no parent -> `primary` (the document stands on its own)
/// - parent + raster-image media class -> `attached_context` (subordinate)
/// - parent + any other / unknown media class -> `attachment` (peer child)
#[must_use]
pub fn derive_document_role(has_parent: bool, is_raster_image: bool) -> &'static str {
    if !has_parent {
        DOCUMENT_ROLE_PRIMARY
    } else if is_raster_image {
        DOCUMENT_ROLE_ATTACHED_CONTEXT
    } else {
        DOCUMENT_ROLE_ATTACHMENT
    }
}

/// Whether a revision's media class is a raster image, derived from the one
/// canonical structural classifier
/// ([`crate::shared::extraction::file_extract::detect_declared_upload_file_kind`]).
/// Callers pass the persisted declared inputs (`file_name`/`external_key` for
/// the extension and the revision `mime_type`); no byte sniffing, no ad-hoc
/// MIME or extension matching outside the canonical classifier.
#[must_use]
pub fn revision_is_raster_image(file_name: Option<&str>, mime_type: Option<&str>) -> bool {
    matches!(
        crate::shared::extraction::file_extract::detect_declared_upload_file_kind(
            file_name, mime_type
        ),
        Some(crate::shared::extraction::file_extract::UploadFileKind::Image)
    )
}

/// Extracts the structural source id of the parent page from an
/// attachment-style structural source value (external key, file name,
/// source uri, or document hint) shaped like `.../download/attachments/<id>/...`.
///
/// This is the canonical parser shared between the document-parentage
/// backfill/resolver and the query-time artifact-sibling heuristic — one
/// implementation so both sides agree on what a structural attachment parent
/// is. It matches a structural URL segment only; it never parses natural
/// language or file extensions.
#[must_use]
pub fn attachment_parent_page_id(value: &str) -> Option<String> {
    let (_, tail) = value.split_once("/download/attachments/")?;
    leading_structural_source_id(tail)
}

/// Extracts the structural source id following a `pageId=` query parameter.
#[must_use]
pub fn source_page_id(value: &str) -> Option<String> {
    let (_, tail) = value.split_once("pageId=")?;
    leading_structural_source_id(tail)
}

/// Reads the leading run of structural-id characters (alphanumerics plus `-`
/// and `_`) from `value`, accepting it only when it is at least two characters
/// long and contains a digit. Shared by [`attachment_parent_page_id`] and
/// [`source_page_id`].
#[must_use]
pub fn leading_structural_source_id(value: &str) -> Option<String> {
    let id = value
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect::<String>();
    if id.len() >= 2 && id.chars().any(|ch| ch.is_ascii_digit()) { Some(id) } else { None }
}

/// Minimum digit length for a numeric structural id to be treated as a
/// page/source identifier. Structural source ids (page ids, attachment owner
/// ids) are long; this floor keeps short numerics (version numbers, ordinals)
/// from being mistaken for a parent identity.
pub const STRUCTURAL_NUMERIC_ID_MIN_DIGITS: usize = 4;

/// Extracts every digit-bounded numeric run of at least
/// [`STRUCTURAL_NUMERIC_ID_MIN_DIGITS`] digits from a structural source value
/// (external key, document hint, source uri). A run is digit-bounded: the
/// characters immediately before and after it are not ASCII digits, so
/// `pageId=125239329`, `confluence:page:20808691`, and `/pages/27531786/Title`
/// all yield their identity id while embedded fragments of longer numbers do
/// not partially match. This is the canonical parent-identity extractor used by
/// the document-parentage resolver to build the page-id -> parent index; it is
/// language-agnostic and reads only numeric structure.
#[must_use]
pub fn structural_source_numeric_ids(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if current.len() >= STRUCTURAL_NUMERIC_ID_MIN_DIGITS {
                ids.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= STRUCTURAL_NUMERIC_ID_MIN_DIGITS {
        ids.push(current);
    }
    ids
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebPageProvenance {
    pub run_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub source_uri: Option<String>,
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentDocumentPipelineJob {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub current_stage: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentDocumentPipelineState {
    pub latest_mutation: Option<ContentMutation>,
    pub latest_job: Option<ContentDocumentPipelineJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentReadinessSummary {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readiness_kind: DocumentReadiness,
    pub activity_status: RuntimeDocumentActivityStatus,
    pub stalled_reason: Option<String>,
    pub preparation_state: String,
    pub graph_coverage_kind: String,
    pub typed_fact_coverage: Option<f64>,
    pub last_mutation_id: Option<Uuid>,
    pub last_job_stage: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryKnowledgeCoverage {
    pub library_id: Uuid,
    pub document_counts_by_readiness: BTreeMap<String, i64>,
    pub graph_ready_document_count: i64,
    pub graph_sparse_document_count: i64,
    pub typed_fact_document_count: i64,
    pub last_generation_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContentDocumentSummary {
    pub document: ContentDocument,
    pub file_name: String,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub source_access: Option<ContentSourceAccess>,
    pub readiness: Option<ContentRevisionReadiness>,
    pub readiness_summary: Option<DocumentReadinessSummary>,
    pub prepared_revision: Option<StructuredDocumentRevision>,
    pub web_page_provenance: Option<WebPageProvenance>,
    pub pipeline: ContentDocumentPipelineState,
}

#[cfg(test)]
mod tests {
    use super::{
        DOCUMENT_ROLE_ATTACHED_CONTEXT, DOCUMENT_ROLE_ATTACHMENT, DOCUMENT_ROLE_PRIMARY,
        attachment_parent_page_id, derive_document_role, effective_document_id,
        leading_structural_source_id, revision_is_raster_image, revision_text_state_is_readable,
        role_is_attached_context, source_page_id, structural_source_numeric_ids,
    };

    #[test]
    fn revision_text_state_is_readable_accepts_canonical_ready_states() {
        assert!(revision_text_state_is_readable("readable"));
        assert!(revision_text_state_is_readable("ready"));
        assert!(revision_text_state_is_readable("text_readable"));
        assert!(!revision_text_state_is_readable("vector_ready"));
        assert!(!revision_text_state_is_readable("graph_ready"));
        assert!(!revision_text_state_is_readable("processing"));
    }

    #[test]
    fn derive_document_role_maps_structural_inputs_to_canonical_roles() {
        // No parent is always a primary document, regardless of media class.
        assert_eq!(derive_document_role(false, false), DOCUMENT_ROLE_PRIMARY);
        assert_eq!(derive_document_role(false, true), DOCUMENT_ROLE_PRIMARY);
        // A raster-image child becomes subordinate attached context.
        assert_eq!(derive_document_role(true, true), DOCUMENT_ROLE_ATTACHED_CONTEXT);
        // A non-image (or unknown) child stays a peer attachment.
        assert_eq!(derive_document_role(true, false), DOCUMENT_ROLE_ATTACHMENT);
    }

    #[test]
    fn effective_document_id_collapses_attached_context_onto_parent() {
        let own = uuid::Uuid::now_v7();
        let parent = uuid::Uuid::now_v7();
        // Attached context with a resolved parent collapses onto the parent.
        assert_eq!(
            effective_document_id(DOCUMENT_ROLE_ATTACHED_CONTEXT, Some(parent), own),
            parent
        );
        // Attached context without a parent falls back to its own id.
        assert_eq!(effective_document_id(DOCUMENT_ROLE_ATTACHED_CONTEXT, None, own), own);
        // Peer attachments and primaries always keep their own id.
        assert_eq!(effective_document_id(DOCUMENT_ROLE_ATTACHMENT, Some(parent), own), own);
        assert_eq!(effective_document_id(DOCUMENT_ROLE_PRIMARY, Some(parent), own), own);
    }

    #[test]
    fn role_is_attached_context_matches_only_the_attached_context_role() {
        assert!(role_is_attached_context(DOCUMENT_ROLE_ATTACHED_CONTEXT));
        assert!(!role_is_attached_context(DOCUMENT_ROLE_ATTACHMENT));
        assert!(!role_is_attached_context(DOCUMENT_ROLE_PRIMARY));
    }

    #[test]
    fn revision_is_raster_image_follows_canonical_classifier() {
        assert!(revision_is_raster_image(Some("diagram.png"), Some("image/png")));
        assert!(revision_is_raster_image(None, Some("image/jpeg")));
        assert!(revision_is_raster_image(Some("photo.gif"), None));
        // Vector graphics are text-like XML, not raster images.
        assert!(!revision_is_raster_image(Some("chart.svg"), Some("image/svg+xml")));
        // Non-image media classes are not raster images.
        assert!(!revision_is_raster_image(Some("manual.pdf"), Some("application/pdf")));
        // Unknown/opaque media class is not classified as a raster image.
        assert!(!revision_is_raster_image(None, Some("application/octet-stream")));
    }

    #[test]
    fn attachment_parent_page_id_reads_structural_segment_only() {
        assert_eq!(
            attachment_parent_page_id("https://host.invalid/download/attachments/4242/img.png"),
            Some("4242".to_string())
        );
        assert_eq!(
            attachment_parent_page_id("/download/attachments/page-77/screenshot.png"),
            Some("page-77".to_string())
        );
        // No structural attachment segment -> no parent id.
        assert_eq!(attachment_parent_page_id("https://host.invalid/pages/4242/view"), None);
    }

    #[test]
    fn source_page_id_reads_page_id_query_parameter() {
        assert_eq!(
            source_page_id("https://host.invalid/viewpage.action?pageId=9100"),
            Some("9100".to_string())
        );
        assert_eq!(source_page_id("https://host.invalid/viewpage.action"), None);
    }

    #[test]
    fn structural_source_numeric_ids_extracts_digit_bounded_long_runs() {
        assert_eq!(
            structural_source_numeric_ids("svc:page:20808691"),
            vec!["20808691".to_string()]
        );
        assert_eq!(
            structural_source_numeric_ids(
                "https://host.invalid/pages/viewpage.action?pageId=125239329"
            ),
            vec!["125239329".to_string()]
        );
        assert_eq!(
            structural_source_numeric_ids("/pages/27531786/title"),
            vec!["27531786".to_string()]
        );
        // Short runs (version numbers, ordinals) are below the floor.
        assert!(structural_source_numeric_ids("UserDoc46/v1/section2").is_empty());
        // Multiple long ids in one value are all returned, in order.
        assert_eq!(
            structural_source_numeric_ids("a=12345&b=67890"),
            vec!["12345".to_string(), "67890".to_string()]
        );
    }

    #[test]
    fn leading_structural_source_id_requires_a_digit_and_min_length() {
        assert_eq!(leading_structural_source_id("4242/rest"), Some("4242".to_string()));
        assert_eq!(leading_structural_source_id("a1-b2?x=1"), Some("a1-b2".to_string()));
        // A pure-alpha leading run has no digit and is rejected.
        assert_eq!(leading_structural_source_id("alpha/rest"), None);
        // A single character is too short.
        assert_eq!(leading_structural_source_id("9/rest"), None);
    }
}
