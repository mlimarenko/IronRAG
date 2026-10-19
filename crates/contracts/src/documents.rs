//! Document lifecycle and ingestion contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::{MessageLevel, OperatorWarning};
use crate::graph::GraphSurface;

/// Fixed protocol capability; absence is not permission to use an older protocol.
pub const CONTENT_VISIBILITY_CONTRACT: &str = "visibility-intent/1";

/// Complete semantic descriptor of replacement bytes and request metadata.
/// Storage paths and inherited document metadata are deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplacementIdentity {
    /// Lowercase SHA-256 prefixed with `sha256:`.
    pub checksum: String,
    /// Exact nonnegative length of replacement bytes.
    pub byte_size: i64,
    /// Replacement media type, normalized independently of prior content.
    pub mime_type: String,
    /// Request filename, not a generated storage path.
    pub file_name: String,
    /// Complete replacement title; null does not inherit an older title.
    pub title: Option<String>,
    /// Complete replacement language declaration.
    pub language_code: Option<String>,
    /// Complete replacement source URI.
    pub source_uri: Option<String>,
    /// Complete replacement extraction hint.
    pub document_hint: Option<String>,
}

/// The field which prevents constructing a canonical replacement identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementIdentityError {
    /// Missing prefix, wrong length or non-lowercase hexadecimal digest.
    Checksum,
    /// Negative replacement length.
    ByteSize,
    /// Empty or control-character-bearing MIME type.
    MimeType,
    /// Empty or control-character-bearing filename.
    FileName,
    /// Extraction hint exceeds its storage boundary.
    DocumentHint,
    /// Empty idempotency key.
    IdempotencyKey,
    /// Empty opaque source identity.
    SourceIdentity,
}

impl std::fmt::Display for ReplacementIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid replacement identity field: {self:?}")
    }
}

impl std::error::Error for ReplacementIdentityError {}

impl ReplacementIdentity {
    /// Constructs the one normalized descriptor used by intent and payload admission.
    /// Missing metadata becomes null; this constructor never reads a prior revision.
    ///
    /// # Errors
    /// Rejects a noncanonical checksum, negative size, empty/control-character MIME
    /// or filename, and document hints exceeding the storage boundary.
    pub fn normalize(mut self) -> Result<Self, ReplacementIdentityError> {
        let digest =
            self.checksum.strip_prefix("sha256:").ok_or(ReplacementIdentityError::Checksum)?;
        if !is_lowercase_hex(digest, 64) {
            return Err(ReplacementIdentityError::Checksum);
        }
        if self.byte_size < 0 {
            return Err(ReplacementIdentityError::ByteSize);
        }
        self.mime_type = self.mime_type.trim().to_ascii_lowercase();
        if self.mime_type.is_empty() || self.mime_type.chars().any(char::is_control) {
            return Err(ReplacementIdentityError::MimeType);
        }
        self.file_name = self.file_name.trim().to_owned();
        if self.file_name.is_empty() || self.file_name.chars().any(char::is_control) {
            return Err(ReplacementIdentityError::FileName);
        }
        self.title = normalized_replacement_metadata(self.title);
        self.language_code = normalized_replacement_metadata(self.language_code);
        self.source_uri = normalized_replacement_metadata(self.source_uri);
        self.document_hint = normalized_replacement_metadata(self.document_hint);
        if self.document_hint.as_ref().is_some_and(|hint| hint.chars().count() > 1024) {
            return Err(ReplacementIdentityError::DocumentHint);
        }
        Ok(self)
    }
}

fn normalized_replacement_metadata(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Immutable revision admission, independent of retained jobs and attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(
    tag = "origin",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RevisionAdmissionIdentity {
    /// A normalized payload admitted through the mutation pipeline.
    Mutation {
        /// Opaque source identity, required for a visibility-bound admission.
        #[serde(deserialize_with = "deserialize_explicit_optional")]
        #[schema(required = true)]
        source_identity: Option<String>,
        /// Required even when null; never reconstructed from operational history.
        #[serde(deserialize_with = "deserialize_explicit_optional")]
        #[schema(required = true)]
        materialization_token: Option<String>,
        /// The exact visibility obligation, or explicit null for unbound work.
        #[serde(deserialize_with = "deserialize_explicit_optional")]
        #[schema(required = true)]
        visibility_intent_id: Option<Uuid>,
        /// Admitted source bytes and metadata, including the request filename.
        replacement: ReplacementIdentity,
    },
    /// An explicit import admission, not a reconstructed ingest attempt.
    CertifiedImport {
        /// Operator-admitted metadata; the filename is an import input.
        replacement: ReplacementIdentity,
        /// Snapshot and operator evidence retained with this admission.
        provenance: ImportAdmissionProvenance,
    },
}

/// Provenance identifiers; maintenance must also verify their identity binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportAdmissionProvenance {
    /// Full lowercase repository snapshot ID.
    pub recovery_snapshot_id: String,
    /// SHA-256 of the admitted identity certificate, without a prefix.
    pub identity_certificate_sha256: String,
    /// SHA-384 of the source baseline, without a prefix.
    pub source_baseline_sha384: String,
    /// Actual admitting operator identity, not a cryptographic signature.
    pub admitted_by: String,
}

/// Invalid revision admission field; error messages never include its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionAdmissionIdentityError {
    /// Invalid replacement metadata.
    Replacement(ReplacementIdentityError),
    /// A visibility-bound admission requires a nonblank source identity.
    SourceIdentity,
    /// Snapshot ID is not a full lowercase SHA-256 identifier.
    RecoverySnapshotId,
    /// Certificate digest is not a full lowercase SHA-256 digest.
    IdentityCertificateSha256,
    /// Source baseline is not a full lowercase SHA-384 digest.
    SourceBaselineSha384,
    /// Missing or control-character-bearing operator identity.
    AdmittedBy,
}

impl std::fmt::Display for RevisionAdmissionIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid revision admission field: {self:?}")
    }
}

impl std::error::Error for RevisionAdmissionIdentityError {}

impl RevisionAdmissionIdentity {
    /// Returns the source descriptor shared by both admission origins.
    #[must_use]
    pub const fn replacement(&self) -> &ReplacementIdentity {
        match self {
            Self::Mutation { replacement, .. } | Self::CertifiedImport { replacement, .. } => {
                replacement
            }
        }
    }

    /// Normalizes replacement metadata while preserving opaque identity values.
    ///
    /// # Errors
    /// Rejects invalid replacement metadata or malformed provenance identifiers.
    /// This validates formats, not snapshot authenticity or certificate binding.
    pub fn normalize(self) -> Result<Self, RevisionAdmissionIdentityError> {
        match self {
            Self::Mutation {
                source_identity,
                materialization_token,
                visibility_intent_id,
                replacement,
            } => {
                if visibility_intent_id.is_some()
                    && source_identity.as_deref().is_none_or(|value| value.trim().is_empty())
                {
                    return Err(RevisionAdmissionIdentityError::SourceIdentity);
                }
                Ok(Self::Mutation {
                    source_identity,
                    materialization_token,
                    visibility_intent_id,
                    replacement: replacement
                        .normalize()
                        .map_err(RevisionAdmissionIdentityError::Replacement)?,
                })
            }
            Self::CertifiedImport { replacement, provenance } => {
                if !is_lowercase_hex(&provenance.recovery_snapshot_id, 64) {
                    return Err(RevisionAdmissionIdentityError::RecoverySnapshotId);
                }
                if !is_lowercase_hex(&provenance.identity_certificate_sha256, 64) {
                    return Err(RevisionAdmissionIdentityError::IdentityCertificateSha256);
                }
                if !is_lowercase_hex(&provenance.source_baseline_sha384, 96) {
                    return Err(RevisionAdmissionIdentityError::SourceBaselineSha384);
                }
                if provenance.admitted_by.trim().is_empty()
                    || provenance.admitted_by.chars().any(char::is_control)
                {
                    return Err(RevisionAdmissionIdentityError::AdmittedBy);
                }
                Ok(Self::CertifiedImport {
                    replacement: replacement
                        .normalize()
                        .map_err(RevisionAdmissionIdentityError::Replacement)?,
                    provenance,
                })
            }
        }
    }
}

/// Denial is admitted separately from replacement work, including when payload is busy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateVisibilityIntentRequest {
    /// Required durable key reused only for retransmission of this observation.
    pub idempotency_key: String,
    /// Required opaque source identity, preserved without rewriting.
    pub source_identity: String,
    /// Required even when null. The backend compares opaque tokens only for equality.
    #[serde(deserialize_with = "deserialize_explicit_optional")]
    #[schema(required = true)]
    pub materialization_token: Option<String>,
    /// Complete SAFE replacement descriptor expected by this obligation.
    pub replacement: ReplacementIdentity,
}

impl CreateVisibilityIntentRequest {
    /// Validates the complete intent boundary without rewriting opaque identities.
    ///
    /// # Errors
    /// Rejects blank request keys/source identities or an invalid replacement.
    pub fn normalize(mut self) -> Result<Self, ReplacementIdentityError> {
        if self.idempotency_key.trim().is_empty() {
            return Err(ReplacementIdentityError::IdempotencyKey);
        }
        if self.source_identity.trim().is_empty() {
            return Err(ReplacementIdentityError::SourceIdentity);
        }
        self.replacement = self.replacement.normalize()?;
        Ok(self)
    }
}

fn deserialize_explicit_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Visibility acceptance receipt, not a revision/job receipt or a currentness promise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VisibilityIntentReceipt {
    /// Visibility item identifier; equal to `mutation_item_id`.
    pub visibility_intent_id: Uuid,
    /// Independent accepted visibility mutation, not the payload owner.
    pub mutation_id: Uuid,
    /// Applied null-result visibility item.
    pub mutation_item_id: Uuid,
    /// Ready visibility operation; no ingest job is implied.
    pub async_operation_id: Uuid,
    /// Document bound by this obligation.
    pub document_id: Uuid,
    /// Retained revision high-water value captured at acceptance.
    pub revision_floor: i32,
    /// Original opaque source identity.
    pub source_identity: String,
    /// Original opaque materialization token; equality only.
    pub materialization_token: Option<String>,
    /// Original normalized expected replacement.
    pub replacement: ReplacementIdentity,
    /// Backend digest of the immutable semantic request.
    pub request_fingerprint: String,
}

#[cfg(test)]
mod visibility_contract_tests {
    use super::{
        CreateVisibilityIntentRequest, ImportAdmissionProvenance, ReplacementIdentity,
        ReplacementIdentityError, RevisionAdmissionIdentity, RevisionAdmissionIdentityError,
    };

    fn replacement() -> ReplacementIdentity {
        ReplacementIdentity {
            checksum: format!("sha256:{}", "a".repeat(64)),
            byte_size: 0,
            mime_type: " TEXT/PLAIN ".to_owned(),
            file_name: " safe.txt ".to_owned(),
            title: Some(" ".to_owned()),
            language_code: None,
            source_uri: None,
            document_hint: Some(" safe ".to_owned()),
        }
    }

    fn import_provenance() -> ImportAdmissionProvenance {
        ImportAdmissionProvenance {
            recovery_snapshot_id: "a".repeat(64),
            identity_certificate_sha256: "b".repeat(64),
            source_baseline_sha384: "c".repeat(96),
            admitted_by: "operator:test".to_owned(),
        }
    }

    #[test]
    fn revision_admission_preserves_opaque_binding_and_normalizes_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = RevisionAdmissionIdentity::Mutation {
            source_identity: Some(" opaque:source ".to_owned()),
            materialization_token: Some(" opaque:materialization ".to_owned()),
            visibility_intent_id: None,
            replacement: replacement(),
        }
        .normalize()?;
        assert_eq!(identity.replacement().file_name, "safe.txt");
        assert_eq!(identity.replacement().mime_type, "text/plain");
        assert_eq!(identity.clone().normalize()?, identity);
        let encoded = serde_json::to_value(&identity)?;
        assert_eq!(encoded["sourceIdentity"], " opaque:source ");
        assert_eq!(encoded["materializationToken"], " opaque:materialization ");
        assert_eq!(serde_json::from_value::<RevisionAdmissionIdentity>(encoded)?, identity);
        Ok(())
    }

    #[test]
    fn revision_admission_requires_explicit_nullable_bindings() -> Result<(), serde_json::Error> {
        let identity = serde_json::json!({
            "origin": "mutation", "sourceIdentity": "source", "materializationToken": null,
            "visibilityIntentId": null, "replacement": replacement(),
        });
        let _: RevisionAdmissionIdentity = serde_json::from_value(identity.clone())?;
        for field in ["sourceIdentity", "materializationToken", "visibilityIntentId"] {
            let mut incomplete = identity.clone();
            incomplete.as_object_mut().map(|object| object.remove(field));
            assert!(serde_json::from_value::<RevisionAdmissionIdentity>(incomplete).is_err());
        }
        let mut mixed = identity;
        mixed["provenance"] = serde_json::to_value(import_provenance())?;
        assert!(serde_json::from_value::<RevisionAdmissionIdentity>(mixed).is_err());
        Ok(())
    }

    #[test]
    fn revision_admission_import_has_no_fabricated_mutation_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = RevisionAdmissionIdentity::CertifiedImport {
            replacement: replacement(),
            provenance: import_provenance(),
        }
        .normalize()?;
        let encoded = serde_json::to_value(&identity)?;
        assert_eq!(encoded["origin"], "certified_import");
        assert_eq!(encoded["replacement"]["fileName"], "safe.txt");
        assert!(encoded.get("materializationToken").is_none());
        assert!(encoded.get("visibilityIntentId").is_none());
        assert!(encoded.get("sourceIdentity").is_none());
        assert_eq!(serde_json::from_value::<RevisionAdmissionIdentity>(encoded.clone())?, identity);
        for field in ["materializationToken", "visibilityIntentId", "sourceIdentity", "attemptId"] {
            let mut forged = encoded.clone();
            forged[field] = serde_json::json!("unissued");
            assert!(serde_json::from_value::<RevisionAdmissionIdentity>(forged).is_err());
        }
        Ok(())
    }

    #[test]
    fn revision_admission_allows_unbound_work_without_a_source_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = RevisionAdmissionIdentity::Mutation {
            source_identity: None,
            materialization_token: None,
            visibility_intent_id: None,
            replacement: replacement(),
        }
        .normalize()?;
        let encoded = serde_json::to_value(&identity)?;
        assert!(encoded["sourceIdentity"].is_null());
        assert_eq!(serde_json::from_value::<RevisionAdmissionIdentity>(encoded)?, identity);
        let bound = RevisionAdmissionIdentity::Mutation {
            source_identity: None,
            materialization_token: None,
            visibility_intent_id: Some(uuid::Uuid::from_u128(1)),
            replacement: replacement(),
        };
        assert_eq!(bound.normalize(), Err(RevisionAdmissionIdentityError::SourceIdentity));
        Ok(())
    }

    #[test]
    fn revision_admission_rejects_malformed_import_provenance() {
        let mut invalid = import_provenance();
        invalid.recovery_snapshot_id = "A".repeat(64);
        let mut invalid_certificate = import_provenance();
        invalid_certificate.identity_certificate_sha256.pop();
        let mut invalid_baseline = import_provenance();
        invalid_baseline.source_baseline_sha384.pop();
        let mut invalid_operator = import_provenance();
        invalid_operator.admitted_by = "operator\nforged".to_owned();
        for (provenance, expected) in [
            (invalid, RevisionAdmissionIdentityError::RecoverySnapshotId),
            (invalid_certificate, RevisionAdmissionIdentityError::IdentityCertificateSha256),
            (invalid_baseline, RevisionAdmissionIdentityError::SourceBaselineSha384),
            (invalid_operator, RevisionAdmissionIdentityError::AdmittedBy),
        ] {
            assert_eq!(
                RevisionAdmissionIdentity::CertifiedImport {
                    replacement: replacement(),
                    provenance
                }
                .normalize(),
                Err(expected)
            );
        }
    }

    #[test]
    fn visibility_contract_normalization_is_idempotent_and_does_not_inherit_metadata()
    -> Result<(), ReplacementIdentityError> {
        let normalized = replacement().normalize()?;
        assert_eq!(normalized.mime_type, "text/plain");
        assert_eq!(normalized.file_name, "safe.txt");
        assert_eq!(normalized.title, None);
        assert_eq!(normalized.document_hint.as_deref(), Some("safe"));
        assert_eq!(normalized.clone().normalize()?, normalized);
        Ok(())
    }

    #[test]
    fn visibility_contract_rejects_invalid_identity_fields() {
        let mut invalid = replacement();
        invalid.checksum = format!("sha256:{}", "A".repeat(64));
        assert_eq!(invalid.normalize(), Err(ReplacementIdentityError::Checksum));
        let mut invalid = replacement();
        invalid.byte_size = -1;
        assert_eq!(invalid.normalize(), Err(ReplacementIdentityError::ByteSize));
        let mut invalid = replacement();
        invalid.file_name = "\r\n".to_owned();
        assert_eq!(invalid.normalize(), Err(ReplacementIdentityError::FileName));
    }

    #[test]
    fn visibility_contract_requires_nullable_token_and_rejects_removed_policy()
    -> Result<(), serde_json::Error> {
        let mut request = serde_json::json!({
            "idempotencyKey": "episode-1", "sourceIdentity": "source", "materializationToken": null,
            "replacement": replacement(),
        });
        let decoded: CreateVisibilityIntentRequest = serde_json::from_value(request.clone())?;
        assert_eq!(decoded.materialization_token, None);
        request.as_object_mut().map(|object| object.remove("materializationToken"));
        assert!(serde_json::from_value::<CreateVisibilityIntentRequest>(request.clone()).is_err());
        request["materializationToken"] = serde_json::Value::Null;
        request["visibilityPolicy"] = serde_json::json!("readable");
        assert!(serde_json::from_value::<CreateVisibilityIntentRequest>(request).is_err());
        Ok(())
    }

    #[test]
    fn visibility_contract_validates_required_keys_without_ordering_opaque_tokens()
    -> Result<(), ReplacementIdentityError> {
        let request = CreateVisibilityIntentRequest {
            idempotency_key: "episode-1".to_owned(),
            source_identity: "source:opaque".to_owned(),
            materialization_token: Some("opaque:Z-before-A".to_owned()),
            replacement: replacement(),
        }
        .normalize()?;
        assert_eq!(request.materialization_token.as_deref(), Some("opaque:Z-before-A"));
        let mut invalid = request.clone();
        invalid.idempotency_key = " ".to_owned();
        assert_eq!(invalid.normalize(), Err(ReplacementIdentityError::IdempotencyKey));
        let mut invalid = request;
        invalid.source_identity.clear();
        assert_eq!(invalid.normalize(), Err(ReplacementIdentityError::SourceIdentity));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Highest query capability currently available for a document.
pub enum DocumentReadiness {
    /// Ingestion has not yet produced readable content.
    Processing,
    /// No mutation, no ingest job, and no readable revision exist: there is
    /// no work in flight and none will start without a new mutation.
    Empty,
    /// Extracted text can be read and searched lexically.
    Readable,
    /// Graph extraction completed with insufficient structured evidence.
    GraphSparse,
    /// Text, vectors, and graph evidence are available.
    GraphReady,
    /// The document could not reach a usable readiness state.
    Failed,
}

impl DocumentReadiness {
    #[must_use]
    /// Returns the stable snake-case value used by storage and transport layers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Processing => "processing",
            Self::Empty => "empty",
            Self::Readable => "readable",
            Self::GraphSparse => "graph_sparse",
            Self::GraphReady => "graph_ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Operational lifecycle of a document ingestion attempt.
pub enum DocumentStatus {
    /// Accepted and waiting for a worker.
    Queued,
    /// A worker is actively ingesting the document.
    Processing,
    /// The current ingestion attempt completed successfully.
    Ready,
    /// The current ingestion attempt ended in failure.
    Failed,
    /// Ingest was cancelled by operator or superseded by a newer
    /// mutation. Distinct from `Failed` — the operator started it
    /// and then withdrew, the document was not rejected by the
    /// pipeline. Rendered with neutral styling in the UI; mixing
    /// cancels into failures was masking real pipeline failures on
    /// the dashboard.
    Canceled,
    /// The document was admitted with no content — no ingest job was
    /// ever created, no revision is readable, and no mutation is
    /// actively running. Distinct from `Queued` — nothing is waiting
    /// for a worker and nothing will move without a new mutation
    /// (e.g. a follow-up revision upload) supplying content.
    Empty,
}

impl DocumentStatus {
    #[must_use]
    /// Returns the stable snake-case value used by storage and transport layers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Empty => "empty",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// List-view metadata and current processing state for one document.
pub struct DocumentSummary {
    /// Stable document identifier.
    pub id: Uuid,
    /// Owning workspace when included by the response surface.
    pub workspace_id: Option<Uuid>,
    /// Owning library when included by the response surface.
    pub library_id: Option<Uuid>,
    /// Immutable published source name, absent when publication is unavailable.
    #[schema(required = true)]
    pub file_name: Option<String>,
    /// Source format or media-type label shown in document lists.
    #[schema(required = true)]
    pub file_type: Option<String>,
    /// Source payload size in bytes.
    #[schema(required = true)]
    pub file_size: Option<i64>,
    /// Time at which the document mutation was accepted.
    pub uploaded_at: DateTime<Utc>,
    /// Operational state of the current ingestion attempt.
    pub status: DocumentStatus,
    /// Highest query capability currently available.
    pub readiness: DocumentReadiness,
    /// Operator-facing name of the active ingestion stage.
    pub stage_label: Option<String>,
    /// Best-effort completion percentage for the active attempt.
    pub progress_percent: Option<i32>,
    /// Attributed provider cost in US dollars when accounting is available.
    pub cost_usd: Option<f64>,
    /// Sanitized explanation of the terminal failure.
    pub failure_message: Option<String>,
    /// Whether the current terminal state supports an operator retry.
    pub can_retry: bool,
    /// Extracted structural segment count when computed.
    pub prepared_segment_count: Option<i32>,
    /// Materialized typed-fact count when computed.
    pub technical_fact_count: Option<i32>,
    /// Normalized format selected by the extraction pipeline.
    pub source_format: Option<String>,
}

/// Per-library document metrics — one authoritative shape for all surfaces.
///
/// Every surface (`/ops/libraries/{id}/dashboard`,
/// `/content/libraries/{id}/documents?includeTotal=true`,
/// `/knowledge/libraries/{id}/summary`) MUST read from this shape.
///
/// Invariants enforced by the shared aggregator:
///   * `total == ready + processing + queued + failed + canceled + empty`
///   * `graph_ready + graph_sparse == ready` (graph readiness is a
///     split of the ready bucket; it does not overlap with processing
///     / queued / failed / canceled).
///
/// `in_flight` is intentionally NOT a field on this struct — it is a
/// derived value `processing + queued`. Adding it would invite the
/// same kind of double-counting drift we just finished removing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryDocumentMetrics {
    /// Sum of all mutually exclusive lifecycle buckets.
    pub total: i64,
    /// Successfully ingested documents.
    pub ready: i64,
    /// Documents with an active worker attempt.
    pub processing: i64,
    /// Documents waiting for a worker attempt.
    pub queued: i64,
    /// Documents whose current attempt failed.
    pub failed: i64,
    /// Documents whose current attempt was canceled or superseded.
    pub canceled: i64,
    /// Documents with no content and no ingest work pending — admitted
    /// with no revision, no readable revision, and no active mutation.
    pub empty: i64,
    /// Ready documents with complete graph materialization.
    pub graph_ready: i64,
    /// Ready documents with sparse graph materialization.
    pub graph_sparse: i64,
    /// Time at which the aggregate was recomputed.
    pub recomputed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Lifecycle of a bounded web-ingestion run.
pub enum WebIngestRunState {
    /// Validated and accepted but not yet discovering pages.
    Accepted,
    /// Traversing eligible links and recording page candidates.
    Discovering,
    /// Fetching or materializing discovered candidates.
    Processing,
    /// All eligible candidates reached a successful terminal outcome.
    Completed,
    /// The run completed with some excluded, blocked, or failed candidates.
    CompletedPartial,
    /// A run-level failure prevented successful completion.
    Failed,
    /// Cancellation was requested before normal completion.
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Candidate counters partitioning the work observed by a web-ingestion run.
pub struct WebRunCounts {
    /// Unique page candidates found during traversal.
    pub discovered: i32,
    /// Candidates admitted by boundary and crawl filters.
    pub eligible: i32,
    /// Candidates whose processing reached a terminal outcome.
    pub processed: i32,
    /// Candidates waiting for processing.
    pub queued: i32,
    /// Candidates with active processing attempts.
    pub processing: i32,
    /// Candidates converged with an existing preferred page.
    pub duplicates: i32,
    /// Candidates rejected by configured materialization filters.
    pub excluded: i32,
    /// Candidates rejected by safety or traversal-boundary policy.
    pub blocked: i32,
    /// Candidates ending in processing failure.
    pub failed: i32,
    /// Candidates canceled before a normal terminal outcome.
    pub canceled: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// One typed URL-matching rule used by web-ingestion filters.
pub struct WebIngestPattern {
    /// Matching strategy understood by the web-ingestion policy.
    pub kind: String,
    /// Pattern payload interpreted according to `kind`.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Origin of the rule when it was inferred or imported.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Ordered admission and rejection rules for candidate URLs.
pub struct WebIngestUrlFilter {
    /// Rules that explicitly admit matching URLs.
    pub allow_patterns: Vec<WebIngestPattern>,
    /// Rules that explicitly reject matching URLs.
    pub block_patterns: Vec<WebIngestPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Configuration, progress, and counters for an existing crawl run.
pub struct WebIngestRunSummary {
    /// Stable run identifier.
    pub run_id: Uuid,
    /// Destination library for materialized pages.
    pub library_id: Uuid,
    /// Traversal mode selected when the run was created.
    pub mode: String,
    /// Origin or path boundary applied during discovery.
    pub boundary_policy: String,
    /// Maximum link distance from the seed page.
    pub max_depth: i32,
    /// Maximum unique page candidates admitted by the run.
    pub max_pages: i32,
    /// URL rules applied before fetching a discovered candidate.
    pub crawl_filter: WebIngestUrlFilter,
    /// URL rules applied before persisting a fetched page as a document.
    pub materialization_filter: WebIngestUrlFilter,
    /// Current run lifecycle.
    pub run_state: WebIngestRunState,
    /// Normalized URL from which traversal began.
    pub seed_url: String,
    /// Current candidate-state counters.
    pub counts: WebRunCounts,
    /// Latest observed progress or terminal-state transition.
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Immediate acknowledgement and current state of a web-run mutation.
pub struct WebIngestRunReceipt {
    /// Stable identifier of the affected run.
    pub run_id: Uuid,
    /// Destination library associated with the run.
    pub library_id: Uuid,
    /// Traversal mode accepted for the run.
    pub mode: String,
    /// Run lifecycle observed after applying the mutation.
    pub run_state: WebIngestRunState,
    /// Candidate counters observed after applying the mutation.
    pub counts: WebRunCounts,
    /// Long-running operation that tracks asynchronous execution.
    pub async_operation_id: Option<Uuid>,
    /// Machine-stable reason when the mutation or run failed.
    pub failure_code: Option<String>,
    /// Time at which cancellation was accepted, if requested.
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Actionable condition highlighted on a library dashboard.
pub struct DashboardAttentionItem {
    /// Machine-stable condition identifier.
    pub code: String,
    /// Short operator-facing summary.
    pub title: String,
    /// Explanation of impact or suggested remediation.
    pub detail: String,
    /// Application route where the condition can be inspected.
    pub route_path: String,
    /// Operational severity of the condition.
    pub level: MessageLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Aggregated library state required by the operational dashboard.
pub struct DashboardSurface {
    /// Mutually exclusive document lifecycle and graph-readiness counts.
    pub document_metrics: LibraryDocumentMetrics,
    /// Most recently mutated documents in the library.
    pub recent_documents: Vec<DocumentSummary>,
    /// Most recently active web-ingestion runs.
    pub recent_web_runs: Vec<WebIngestRunSummary>,
    /// Current graph topology and readiness summary.
    pub graph: GraphSurface,
    /// Conditions that may require operator action.
    pub attention: Vec<DashboardAttentionItem>,
    /// Non-fatal conditions affecting dashboard completeness or accuracy.
    pub warnings: Vec<OperatorWarning>,
}
