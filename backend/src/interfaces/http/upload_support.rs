use uuid::Uuid;

use crate::{
    interfaces::http::router_support::ApiError,
    services::runtime_ingestion::{QueueRuntimeUploadRequest, RuntimeUploadFileInput},
    shared::file_extract::UploadAdmissionError,
};

#[derive(Debug, Clone)]
pub struct MultipartUploadFileInput {
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

pub fn build_runtime_upload_requests(
    project_id: Uuid,
    upload_batch_id: Uuid,
    requested_by: Option<String>,
    trigger_kind: &str,
    upload_limit_mb: u64,
    files: Vec<MultipartUploadFileInput>,
) -> Result<Vec<QueueRuntimeUploadRequest>, ApiError> {
    if files.is_empty() {
        return Err(ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file(
            "no files were uploaded",
        )));
    }

    let upload_limit_bytes = upload_limit_mb.saturating_mul(1024 * 1024);
    let mut requests = Vec::with_capacity(files.len());
    for file in files {
        let file_size_bytes = u64::try_from(file.file_bytes.len()).unwrap_or(u64::MAX);
        if file_size_bytes > upload_limit_bytes {
            return Err(ApiError::from_upload_admission(UploadAdmissionError::file_too_large(
                &file.file_name,
                file.mime_type.as_deref(),
                file_size_bytes,
                upload_limit_mb,
            )));
        }

        requests.push(QueueRuntimeUploadRequest {
            project_id,
            upload_batch_id: Some(upload_batch_id),
            requested_by: requested_by.clone(),
            trigger_kind: trigger_kind.to_string(),
            parent_job_id: None,
            idempotency_key: None,
            file: RuntimeUploadFileInput {
                source_id: None,
                file_name: file.file_name,
                mime_type: file.mime_type,
                file_bytes: file.file_bytes,
                title: None,
            },
        });
    }

    Ok(requests)
}

#[cfg(test)]
mod tests {
    use super::{MultipartUploadFileInput, build_runtime_upload_requests};
    use crate::interfaces::http::router_support::ApiError;

    #[test]
    fn accepts_mixed_batch_with_pdf_and_large_txt_under_limit() {
        let project_id = uuid::Uuid::now_v7();
        let upload_batch_id = uuid::Uuid::now_v7();
        let requests = build_runtime_upload_requests(
            project_id,
            upload_batch_id,
            Some("operator@example.com".to_string()),
            "ui_upload",
            2,
            vec![
                MultipartUploadFileInput {
                    file_name: "manual.pdf".to_string(),
                    mime_type: Some("application/pdf".to_string()),
                    file_bytes: b"%PDF-1.7".to_vec(),
                },
                MultipartUploadFileInput {
                    file_name: "notes.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes: vec![b'a'; 512 * 1024],
                },
            ],
        )
        .expect("mixed batch accepted");

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].project_id, project_id);
        assert_eq!(requests[0].file.file_name, "manual.pdf");
        assert_eq!(requests[1].file.file_name, "notes.txt");
        assert_eq!(requests[1].upload_batch_id, Some(upload_batch_id));
    }

    #[test]
    fn rejects_oversized_text_in_mixed_batch_with_structured_details() {
        let error = build_runtime_upload_requests(
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            Some("operator@example.com".to_string()),
            "ui_upload",
            1,
            vec![
                MultipartUploadFileInput {
                    file_name: "manual.pdf".to_string(),
                    mime_type: Some("application/pdf".to_string()),
                    file_bytes: b"%PDF-1.7".to_vec(),
                },
                MultipartUploadFileInput {
                    file_name: "large-notes.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes: vec![b'a'; 2 * 1024 * 1024],
                },
            ],
        )
        .expect_err("oversized batch rejected");

        match error {
            ApiError::UploadRejected { error_kind, details, .. } => {
                assert_eq!(error_kind, "upload_limit_exceeded");
                assert_eq!(details.file_name.as_deref(), Some("large-notes.txt"));
                assert_eq!(details.detected_format.as_deref(), Some("Text"));
                assert_eq!(details.upload_limit_mb, Some(1));
            }
            other => panic!("expected structured upload rejection, got {other:?}"),
        }
    }
}
