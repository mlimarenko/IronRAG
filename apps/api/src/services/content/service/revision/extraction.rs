use super::*;
use crate::integrations::{extraction_protocol::Extraction, xberg};
use crate::shared::extraction::file_extract::{
    VisualRecognitionInput, VisualRecognitionReceipt, build_model_free_file_extraction_plan,
    build_plan_from_extraction, recognize_native_visuals, xberg_failure_class,
    xberg_owned_recognition_profile, xberg_source_format,
};

async fn resolve_library_recognition_policy(
    state: &AppState,
    library_id: Uuid,
) -> Result<LibraryRecognitionPolicy, String> {
    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|_| "failed to load library recognition policy".to_owned())?
        .ok_or_else(|| format!("library {library_id} was not found"))?;
    LibraryRecognitionPolicy::from_json(row.recognition_policy)
}

impl ContentService {
    pub(crate) fn xberg_vision_binding_identity(
        binding: &ResolvedRuntimeBinding,
    ) -> Result<serde_json::Value, serde_json::Error> {
        use sha2::{Digest as _, Sha256};
        let parameters = serde_json::to_vec(&binding.extra_parameters_json)?;
        Ok(serde_json::json!({
            "binding_id":binding.binding_id,"account_id":binding.account_id,
            "provider_catalog_id":binding.provider_catalog_id,"model_catalog_id":binding.model_catalog_id,
            "provider":binding.provider_kind,"model":binding.model_name,
            "api_style":binding.provider_api_style,
            "parameters_sha256":hex::encode(Sha256::digest(parameters)),
            "endpoint_sha256":hex::encode(Sha256::digest(binding.provider_base_url.as_deref().unwrap_or_default().as_bytes())),
            "prompt_sha256":hex::encode(Sha256::digest(crate::shared::extraction::image::IMAGE_OCR_PROMPT.as_bytes())),
        }))
    }

    pub async fn build_runtime_extraction_plan(
        state: &AppState,
        library_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let size = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        let rejection = |error: FileExtractError| {
            UploadAdmissionError::from_file_extract_error(file_name, mime_type, size, &error)
        };
        let kind = validate_upload_file_admission(Some(file_name), mime_type, file_bytes)
            .map_err(&rejection)?;
        if let Some(source_format) = xberg_source_format(kind, Some(file_name), mime_type) {
            let source =
                xberg::XbergSource { file_name: Some(file_name), mime_type, source_format };
            let mut native =
                xberg::extract_native(source, file_bytes.to_vec()).await.map_err(|error| {
                    rejection(FileExtractError::ExtractionFailed {
                        file_kind: kind,
                        class: xberg_failure_class(&error),
                        message: error.to_string(),
                    })
                })?;
            let receipt = Self::recognize_runtime_native_output(
                state,
                library_id,
                kind,
                &mut native,
                None,
                cancellation_token,
            )
            .await
            .map_err(&rejection)?;
            let calls = receipt.calls.clone();
            let mut output = xberg::project_native(native, source).map_err(|_| {
                rejection(FileExtractError::VisionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::OperationalTerminal,
                    message: "native extraction projection failed".into(),
                    calls: calls.clone(),
                })
            })?;
            receipt.apply(&mut output);
            if output.content_text.trim().is_empty() {
                return Err(rejection(FileExtractError::VisionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::ContentInvalid,
                    message: "document contains no usable native or recognized text".into(),
                    calls,
                }));
            }
            return Ok(build_plan_from_extraction(
                kind,
                output,
                xberg_owned_recognition_profile(kind, source_format).map_err(&rejection)?,
            ));
        }
        if kind != UploadFileKind::Image {
            return build_model_free_file_extraction_plan(
                Some(file_name),
                mime_type,
                file_bytes.to_vec(),
            )
            .await
            .map_err(rejection);
        }
        let policy =
            resolve_library_recognition_policy(state, library_id).await.map_err(|message| {
                rejection(FileExtractError::ExtractionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::OperationalRetryable,
                    message,
                })
            })?;
        let binding = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|_| {
                rejection(FileExtractError::ExtractionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::OperationalRetryable,
                    message: "failed to resolve extract_text binding".into(),
                })
            })?;
        let provider = binding.as_ref().map(|binding| ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        });
        let binding_identity =
            binding.as_ref().map(Self::xberg_vision_binding_identity).transpose().map_err(
                |_| {
                    rejection(FileExtractError::ExtractionFailed {
                        file_kind: kind,
                        class: ExtractionFailureClass::OperationalTerminal,
                        message: "cannot establish effective Vision binding identity".into(),
                    })
                },
            )?;
        let mut plan = build_runtime_file_extraction_plan(FileExtractionRequest {
            cancellation_token: Some(cancellation_token),
            gateway: state.llm_gateway.as_ref(),
            vision_provider: provider.as_ref(),
            vision_api_key: binding.as_ref().and_then(|binding| binding.api_key.as_deref()),
            vision_base_url: binding
                .as_ref()
                .and_then(|binding| binding.provider_base_url.as_deref()),
            vision_extra_parameters_json: binding
                .as_ref()
                .map(|binding| &binding.extra_parameters_json),
            file_name: Some(file_name),
            mime_type,
            file_bytes: file_bytes.to_vec(),
            recognition_policy: &policy,
        })
        .await
        .map_err(&rejection)?;
        if let Some(identity) = binding_identity
            && let Some(outcomes) = plan
                .source_map
                .get_mut("visual_recognition")
                .and_then(serde_json::Value::as_array_mut)
        {
            for outcome in outcomes {
                outcome["binding"] = identity.clone();
            }
        }
        Ok(plan)
    }

    pub async fn recognize_runtime_native_output(
        state: &AppState,
        library_id: Uuid,
        kind: UploadFileKind,
        native: &mut Extraction,
        expected_identity: Option<&serde_json::Value>,
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> Result<VisualRecognitionReceipt, FileExtractError> {
        // Native-only documents must not require model readiness or an active binding.
        if native.visual_inputs.is_empty() {
            return Ok(VisualRecognitionReceipt::default());
        }
        let failure = |message: &str| FileExtractError::ExtractionFailed {
            file_kind: kind,
            class: ExtractionFailureClass::OperationalTerminal,
            message: message.into(),
        };
        let policy = resolve_library_recognition_policy(state, library_id)
            .await
            .map_err(|_| failure("cannot establish current recognition policy"))?;
        if let Some(expected) = expected_identity.and_then(|identity| identity.get("policy"))
            && policy.to_json().as_ref().ok() != Some(expected)
        {
            return Err(failure(
                "recognition policy changed during extraction; create a controlled new revision",
            ));
        }
        let binding = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|_| failure("cannot resolve current extract_text binding"))?
            .ok_or_else(|| failure("visual recognition requires an active extract_text binding"))?;
        let binding_identity = Self::xberg_vision_binding_identity(&binding)
            .map_err(|_| failure("cannot establish effective Vision binding identity"))?;
        if let Some(expected) =
            expected_identity.and_then(|identity| identity.get("vision_binding"))
            && &binding_identity != expected
        {
            return Err(failure(
                "Vision binding changed during extraction; create a controlled new revision",
            ));
        }
        let provider = ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        };
        let mut receipt = recognize_native_visuals(
            native,
            kind,
            VisualRecognitionInput {
                gateway: state.llm_gateway.as_ref(),
                vision_provider: Some(&provider),
                api_key: binding.api_key.as_deref().unwrap_or_default(),
                base_url: binding.provider_base_url.as_deref(),
                extra_parameters_json: Some(&binding.extra_parameters_json),
                cancellation_token: Some(cancellation_token),
            },
        )
        .await?;
        for outcome in &mut receipt.outcomes {
            outcome["binding"] = binding_identity.clone();
        }
        Ok(receipt)
    }

    pub(crate) fn validate_inline_file_admission(
        &self,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<UploadFileKind, ApiError> {
        let size = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        validate_upload_file_admission(Some(file_name), mime_type, file_bytes).map_err(|error| {
            ApiError::from_upload_admission(UploadAdmissionError::from_file_extract_error(
                file_name, mime_type, size, &error,
            ))
        })
    }
}

#[cfg(test)]
#[path = "extraction_tests.rs"]
mod tests;
