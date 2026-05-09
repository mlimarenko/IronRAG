use crate::domains::ops::{OpsAsyncOperation, OpsAsyncOperationStatus};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

#[test]
fn async_operation_serializes_using_canonical_camel_case_fields() -> Result<(), serde_json::Error> {
    let operation = OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Some(Uuid::now_v7()),
        operation_kind: "content_mutation".to_string(),
        status: OpsAsyncOperationStatus::Ready,
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("content_mutation".to_string()),
        subject_id: Some(Uuid::now_v7()),
        parent_async_operation_id: None,
        failure_code: None,
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
    };

    let serialized = serde_json::to_value(&operation)?;

    assert!(serialized.get("completedAt").is_some());
    assert!(serialized.get("completed_at").is_none());
    assert_eq!(serialized.get("status"), Some(&json!("ready")));
    Ok(())
}
