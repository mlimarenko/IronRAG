mod api_error;
mod audit_event;
mod problem_response;
mod runtime_rows;
mod service_error_mapping;
mod vector_search;

pub use api_error::ApiError;
pub(crate) use audit_event::{RestAuditEvent, record_rest_audit_event};
pub use problem_response::{
    ApiErrorBody, PROBLEM_JSON_CONTENT_TYPE, REQUEST_ID_HEADER, RequestId,
    attach_request_id_header, ensure_or_generate_request_id,
};
pub use runtime_rows::{
    map_runtime_action_record_row, map_runtime_execution_row, map_runtime_policy_decision_row,
    map_runtime_stage_record_row, map_runtime_trace_view,
};
pub use service_error_mapping::{
    map_library_create_error, map_runtime_lifecycle_error, map_workspace_create_error,
};
pub(crate) use vector_search::current_embedding_search_guard;
