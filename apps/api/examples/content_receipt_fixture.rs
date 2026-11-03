//! Native wire fixtures for connector admission and current-head product tests.

use chrono::DateTime;
use ironrag_backend::{
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState, ContentMutation,
        ContentMutationItem,
    },
    interfaces::http::content::types::{
        ContentDocumentDetailResponse, ContentMutationDetailResponse, CreateDocumentResponse,
    },
};
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn admission(bound: bool) -> ContentMutationDetailResponse {
    ContentMutationDetailResponse {
        mutation: ContentMutation {
            id: id(4),
            workspace_id: id(1),
            library_id: id(2),
            operation_kind: "replace".to_owned(),
            mutation_state: "accepted".to_owned(),
            requested_at: DateTime::UNIX_EPOCH,
            completed_at: None,
            requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            idempotency_key: Some("native-payload-key".to_owned()),
            source_identity: None,
            failure_code: None,
            conflict_code: None,
        },
        items: vec![ContentMutationItem {
            id: id(5),
            mutation_id: id(4),
            document_id: Some(id(3)),
            base_revision_id: None,
            result_revision_id: Some(id(6)),
            item_state: "pending".to_owned(),
            message: None,
        }],
        job_id: Some(id(7)),
        async_operation_id: Some(id(8)),
        operation_status: Some("accepted".to_owned()),
        result_revision_id: Some(id(6)),
        result_checksum: Some(format!("sha256:{}", "a".repeat(64))),
        visibility_intent_id: bound.then(|| id(9)),
        request_fingerprint: Some("opaque-native-fingerprint".to_owned()),
    }
}

fn detail(bound: bool, published: bool) -> ContentDocumentDetailResponse {
    ContentDocumentDetailResponse {
        document: ContentDocument {
            id: id(3),
            workspace_id: id(1),
            library_id: id(2),
            external_key: "native:page:1".to_owned(),
            document_state: "active".to_owned(),
            created_at: DateTime::UNIX_EPOCH,
        },
        file_name: Some("page.txt".to_owned()),
        source_access: None,
        head: Some(ContentDocumentHead {
            document_id: id(3),
            visibility_intent_id: bound.then(|| id(9)),
            active_revision_id: Some(id(6)),
            readable_revision_id: published.then(|| id(6)),
            latest_mutation_id: Some(id(4)),
            latest_successful_attempt_id: None,
            head_updated_at: DateTime::UNIX_EPOCH,
            document_summary: None,
        }),
        active_revision: None,
        readiness: None,
        readiness_summary: None,
        prepared_segment_count: None,
        technical_fact_count: None,
        web_page_provenance: None,
        pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
        lifecycle: None,
    }
}

fn main() -> Result<(), serde_json::Error> {
    let mut metadata_mutation = admission(false);
    metadata_mutation.mutation.operation_kind = "upload".to_owned();
    metadata_mutation.items.clear();
    metadata_mutation.result_revision_id = None;
    metadata_mutation.result_checksum = None;
    metadata_mutation.job_id = None;
    let mut metadata_document = detail(false, false);
    metadata_document.head = None;
    let fixtures = serde_json::json!({
        "ordinary": serde_json::to_string(&admission(false))?,
        "bound": serde_json::to_string(&admission(true))?,
        "create": serde_json::to_string(&CreateDocumentResponse { document: detail(false, false), mutation: admission(false) })?,
        "published": serde_json::to_string(&detail(true, true))?,
        "suppressed": serde_json::to_string(&detail(true, false))?,
        "metadata_create": serde_json::to_string(&CreateDocumentResponse {
            document: metadata_document, mutation: metadata_mutation,
        })?,
    });
    serde_json::to_writer(std::io::stdout(), &fixtures)
}
