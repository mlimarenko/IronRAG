use super::*;

/// Inputs for [`ContentService::build_runtime_pdf_docling_extraction_plan_from_output`].
/// The already-extracted docling output travels with the source identity it was
/// extracted for, so the plan builder reads as one cohesive unit.
pub(crate) struct DoclingPdfExtractionPlanInput<'a> {
    pub library_id: Uuid,
    pub file_name: &'a str,
    pub mime_type: Option<&'a str>,
    pub file_size_bytes: u64,
    pub output: ExtractionOutput,
}

async fn resolve_library_recognition_policy(
    state: &AppState,
    library_id: Uuid,
) -> Result<LibraryRecognitionPolicy, String> {
    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|error| format!("failed to load library recognition policy: {error}"))?
        .ok_or_else(|| format!("library {library_id} was not found"))?;
    LibraryRecognitionPolicy::from_json(row.recognition_policy)
}

impl ContentService {
    pub async fn build_runtime_extraction_plan(
        &self,
        state: &AppState,
        library_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        let recognition_policy =
            resolve_library_recognition_policy(state, library_id).await.map_err(|error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &FileExtractError::ExtractionFailed {
                        file_kind: validate_upload_file_admission(
                            Some(file_name),
                            mime_type,
                            file_bytes,
                        )
                        .unwrap_or(UploadFileKind::Binary),
                        // Reading the library recognition policy is a database
                        // read on the live ingest path: operational, not a
                        // statement about the payload.
                        class: ExtractionFailureClass::OperationalRetryable,
                        message: error,
                    },
                )
            })?;
        let vision_binding = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &FileExtractError::ExtractionFailed {
                        file_kind: validate_upload_file_admission(
                            Some(file_name),
                            mime_type,
                            file_bytes,
                        )
                        .unwrap_or(UploadFileKind::Binary),
                        // `Err` from binding resolution is a database fault; an
                        // absent binding is `Ok(None)` and is handled as
                        // "no vision", not as this error.
                        class: ExtractionFailureClass::OperationalRetryable,
                        message: format!(
                            "failed to resolve document-understanding binding: {error}"
                        ),
                    },
                )
            })?;
        let vision_provider = vision_binding.as_ref().map(|binding| ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        });
        let plan = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: state.llm_gateway.as_ref(),
            vision_provider: vision_provider.as_ref(),
            vision_api_key: vision_binding.as_ref().and_then(|binding| binding.api_key.as_deref()),
            vision_base_url: vision_binding
                .as_ref()
                .and_then(|binding| binding.provider_base_url.as_deref()),
            vision_extra_parameters_json: vision_binding
                .as_ref()
                .map(|binding| &binding.extra_parameters_json),
            file_name: Some(file_name),
            mime_type,
            file_bytes: file_bytes.to_vec(),
            recognition_policy: &recognition_policy,
        })
        .await
        .map_err(|error| {
            UploadAdmissionError::from_file_extract_error(
                file_name,
                mime_type,
                file_size_bytes,
                &error,
            )
        })?;
        Ok(plan)
    }

    pub(crate) async fn build_runtime_pdf_docling_extraction_plan_from_output(
        &self,
        state: &AppState,
        input: DoclingPdfExtractionPlanInput<'_>,
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let DoclingPdfExtractionPlanInput {
            library_id,
            file_name,
            mime_type,
            file_size_bytes,
            mut output,
        } = input;
        self.augment_runtime_docling_output(state, library_id, &mut output).await.map_err(
            |error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &error,
                )
            },
        )?;

        let plan = build_docling_pdf_extraction_plan(output);
        Ok(plan)
    }

    pub async fn augment_runtime_docling_output(
        &self,
        state: &AppState,
        library_id: Uuid,
        output: &mut ExtractionOutput,
    ) -> Result<(), FileExtractError> {
        let recognition_policy = match resolve_library_recognition_policy(state, library_id).await {
            Ok(policy) => policy,
            Err(error) => {
                output.extracted_images.clear();
                return Err(FileExtractError::ExtractionFailed {
                    file_kind: UploadFileKind::Pdf,
                    class: ExtractionFailureClass::OperationalRetryable,
                    message: format!(
                        "failed to load recognition policy for docling embedded picture OCR: {error}"
                    ),
                });
            }
        };
        let vision_binding = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|error| {
                output.extracted_images.clear();
                FileExtractError::ExtractionFailed {
                    file_kind: UploadFileKind::Pdf,
                    class: ExtractionFailureClass::OperationalRetryable,
                    message: format!("failed to resolve document-understanding binding: {error}"),
                }
            })?;
        if !docling_embedded_picture_vision_enabled(&recognition_policy, vision_binding.is_some()) {
            append_docling_picture_ocr_fallback(output);
            output.extracted_images.clear();
            return Ok(());
        }
        let Some(vision_binding) = vision_binding else {
            output.extracted_images.clear();
            return Ok(());
        };
        let vision_provider = ProviderModelSelection {
            provider_kind: vision_binding.provider_kind.clone(),
            model_name: vision_binding.model_name.clone(),
        };
        augment_docling_output_with_vision_picture_ocr(
            output,
            UploadFileKind::Pdf,
            VisionOcrInput {
                gateway: state.llm_gateway.as_ref(),
                vision_provider: Some(&vision_provider),
                api_key: vision_binding.api_key.as_deref(),
                base_url: vision_binding.provider_base_url.as_deref(),
                extra_parameters_json: Some(&vision_binding.extra_parameters_json),
            },
        )
        .await
    }

    pub(crate) fn validate_inline_file_admission(
        &self,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<UploadFileKind, ApiError> {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        validate_upload_file_admission(Some(file_name), mime_type, file_bytes).map_err(|error| {
            ApiError::from_upload_admission(UploadAdmissionError::from_file_extract_error(
                file_name,
                mime_type,
                file_size_bytes,
                &error,
            ))
        })
    }
}

#[cfg(test)]
#[path = "extraction_tests.rs"]
mod tests;
