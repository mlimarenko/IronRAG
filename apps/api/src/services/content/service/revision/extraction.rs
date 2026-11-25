use super::*;
use crate::integrations::{extraction_protocol::Extraction, xberg};
use crate::shared::extraction::file_extract::{
    VisualRecognitionInput, VisualRecognitionReceipt, build_model_free_file_extraction_plan,
    build_plan_from_extraction, recognize_native_visuals_with_ledger, xberg_failure_class,
    xberg_owned_recognition_profile, xberg_source_format,
};

use crate::services::ops::billing::provider_reservation::ProviderCallReservation;
use crate::shared::extraction::image::{VisionCallLedger, VisionLedgerRequest};
use crate::shared::provider_send::ProviderSendScope;

/// Reservation owner of the paid Vision attempts of one ingest attempt.
///
/// The guards are the canonical [`ProviderCallReservation`], keyed by the exact
/// provider-call id reported to the recognition boundary. Each guard owns the
/// recovery state of its row: reserved-but-unused, or observed with the exact
/// usage payload.
struct IngestVisionLedger<'a> {
    state: &'a AppState,
    authority: IngestRevisionWriteAuthority,
    binding: &'a ResolvedRuntimeBinding,
    reservations: std::sync::Mutex<std::collections::HashMap<Uuid, ProviderCallReservation>>,
}

impl<'a> IngestVisionLedger<'a> {
    fn new(
        state: &'a AppState,
        authority: IngestRevisionWriteAuthority,
        binding: &'a ResolvedRuntimeBinding,
    ) -> Self {
        Self {
            state,
            authority,
            binding,
            reservations: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Takes the guard that owns `provider_call_id`, transferring recovery to
    /// the caller.
    ///
    /// The guard leaves the map before its completion await, so the local
    /// binding still owns the exact usage or the pre-response terminal state if
    /// that await is cancelled or fails. An id this ledger never reserved is
    /// refused instead of being fabricated or replayed.
    fn take_reservation(&self, provider_call_id: Uuid) -> Result<ProviderCallReservation, String> {
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&provider_call_id)
            .ok_or_else(|| {
                format!("provider call {provider_call_id} has no active Vision reservation")
            })
    }
}

#[async_trait::async_trait]
impl VisionCallLedger for IngestVisionLedger<'_> {
    async fn reserve(
        &self,
        request: VisionLedgerRequest<'_>,
    ) -> Result<(Uuid, ProviderSendScope), String> {
        if request.provider_kind != self.binding.provider_kind
            || request.model_name != self.binding.model_name
        {
            return Err("Vision request does not match the resolved binding".to_owned());
        }
        // One durable reservation per physical attempt, owned by the ingest
        // attempt itself. The constructor derives the closed ingest call kind
        // from the resolved binding purpose, so an unsupported purpose is
        // refused before a row exists.
        let reservation = ProviderCallReservation::reserve_ingest_attempt(
            self.state,
            self.authority.attempt_id,
            self.authority.workspace_id,
            self.authority.library_id,
            self.binding,
        )
        .await
        .map_err(|error| error.to_string())?;
        let provider_call_id = reservation.provider_call_id();
        let scope = reservation.send_scope();
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider_call_id, reservation);
        Ok((provider_call_id, scope))
    }

    async fn settle(
        &self,
        provider_call_id: Uuid,
        usage: &serde_json::Value,
    ) -> Result<(), String> {
        let mut reservation = self.take_reservation(provider_call_id)?;
        reservation.complete(usage).await.map_err(|error| error.to_string())
    }

    async fn fail(&self, provider_call_id: Uuid) -> Result<(), String> {
        let mut reservation = self.take_reservation(provider_call_id)?;
        reservation.fail().await.map_err(|error| error.to_string())
    }
}

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

/// The uploaded bytes a runtime extraction plan is built for, with the exact
/// revision authority the plan will run under.
pub struct RuntimeExtractionUpload<'a> {
    pub library_id: Uuid,
    pub authority: IngestRevisionWriteAuthority,
    pub file_name: &'a str,
    pub mime_type: Option<&'a str>,
    pub file_bytes: &'a [u8],
}

/// A native extraction this recognition pass reads, together with the identity
/// it must still match.
pub struct NativeRecognitionTarget<'a> {
    pub library_id: Uuid,
    pub kind: UploadFileKind,
    pub native: &'a mut Extraction,
    pub expected_identity: Option<&'a serde_json::Value>,
    pub authority: Option<IngestRevisionWriteAuthority>,
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
        upload: RuntimeExtractionUpload<'_>,
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let RuntimeExtractionUpload { library_id, authority, file_name, mime_type, file_bytes } =
            upload;
        let size = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        let rejection = |error: FileExtractError| {
            UploadAdmissionError::from_file_extract_error(file_name, mime_type, size, &error)
        };
        let kind = validate_upload_file_admission(Some(file_name), mime_type, file_bytes)
            .map_err(rejection)?;
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
                NativeRecognitionTarget {
                    library_id,
                    kind,
                    native: &mut native,
                    expected_identity: None,
                    authority: Some(authority),
                },
                cancellation_token,
            )
            .await
            .map_err(rejection)?;
            let calls = receipt.calls.clone();
            let mut output = xberg::project_native(native, source).map_err(|_| {
                rejection(FileExtractError::VisionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::OperationalTerminal,
                    message: "native extraction projection failed".into(),
                    calls: calls.clone(),
                    provider_call_id: None,
                })
            })?;
            receipt.apply(&mut output);
            if output.content_text.trim().is_empty() {
                return Err(rejection(FileExtractError::VisionFailed {
                    file_kind: kind,
                    class: ExtractionFailureClass::ContentInvalid,
                    message: "document contains no usable native or recognized text".into(),
                    calls,
                    provider_call_id: None,
                }));
            }
            return Ok(build_plan_from_extraction(
                kind,
                output,
                xberg_owned_recognition_profile(kind, source_format).map_err(rejection)?,
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
        let dispatch = binding.as_ref().map(|binding| binding.dispatch_authority());
        let ledger =
            binding.as_ref().map(|binding| IngestVisionLedger::new(state, authority, binding));
        let mut plan = build_runtime_file_extraction_plan(FileExtractionRequest {
            vision_ledger: ledger.as_ref().map(|ledger| ledger as &dyn VisionCallLedger),
            vision_dispatch: dispatch.as_ref(),
            max_output_tokens_override: binding
                .as_ref()
                .and_then(|binding| binding.max_output_tokens_override),
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
        .map_err(rejection)?;
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
        target: NativeRecognitionTarget<'_>,
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> Result<VisualRecognitionReceipt, FileExtractError> {
        let NativeRecognitionTarget { library_id, kind, native, expected_identity, authority } =
            target;
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
        let dispatch = binding.dispatch_authority();
        let ledger = authority.map(|authority| IngestVisionLedger::new(state, authority, &binding));
        let mut receipt = recognize_native_visuals_with_ledger(
            native,
            kind,
            VisualRecognitionInput {
                dispatch: Some(&dispatch),
                max_output_tokens_override: binding.max_output_tokens_override,
                gateway: state.llm_gateway.as_ref(),
                vision_provider: Some(&provider),
                api_key: binding.api_key.as_deref().unwrap_or_default(),
                base_url: binding.provider_base_url.as_deref(),
                extra_parameters_json: Some(&binding.extra_parameters_json),
                cancellation_token: Some(cancellation_token),
            },
            ledger.as_ref().map(|ledger| ledger as &dyn VisionCallLedger),
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
