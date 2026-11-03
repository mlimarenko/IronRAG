use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest as _, Sha256};

use crate::{
    domains::provider_profiles::ProviderModelSelection,
    integrations::{
        extraction_protocol::{Extraction, MAX_TEXT, VisualInputKind},
        llm::LlmGateway,
    },
    shared::extraction::{
        ExtractionOutput,
        image::{self, VisionCallReceipt, VisualTextOutcome},
    },
};

use super::{ExtractionFailureClass, FileExtractError, UploadFileKind};

pub struct VisualRecognitionInput<'a> {
    pub gateway: &'a dyn LlmGateway,
    pub vision_provider: Option<&'a ProviderModelSelection>,
    pub api_key: &'a str,
    pub base_url: Option<&'a str>,
    pub extra_parameters_json: Option<&'a serde_json::Value>,
    pub cancellation_token: Option<&'a tokio_util::sync::CancellationToken>,
}

#[derive(Default)]
pub struct VisualRecognitionReceipt {
    pub outcomes: Vec<serde_json::Value>,
    pub calls: Vec<VisionCallReceipt>,
    pub warnings: Vec<String>,
}

impl VisualRecognitionReceipt {
    pub fn apply(self, output: &mut ExtractionOutput) {
        if let Some(call) = self.calls.first() {
            output.provider_kind = Some(call.provider_kind.clone());
            output.model_name = Some(call.model_name.clone());
        }
        output.source_map["visual_recognition"] = serde_json::json!(self.outcomes);
        output.usage_json = serde_json::json!({"vision_calls": self.calls});
        output.warnings.extend(self.warnings);
    }
}

pub(crate) fn aggregate_vision_usage_json(usages: Vec<serde_json::Value>) -> serde_json::Value {
    let calls = usages
        .into_iter()
        .flat_map(|usage| {
            usage
                .get("vision_calls")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    serde_json::json!({"vision_calls":calls})
}

fn invalid_visual(file_kind: UploadFileKind) -> FileExtractError {
    FileExtractError::ExtractionFailed {
        file_kind,
        class: ExtractionFailureClass::OperationalTerminal,
        message: "native visual input has invalid identity or source-unit association".into(),
    }
}

/// The native parent validates producer output. Validate again at the paid boundary,
/// before sending any bytes or modifying units. Nothing is published on partial failure.
pub async fn recognize_native_visuals(
    native: &mut Extraction,
    file_kind: UploadFileKind,
    input: VisualRecognitionInput<'_>,
) -> Result<VisualRecognitionReceipt, FileExtractError> {
    let text_limit = native.text_byte_limit;
    let text_bytes = native
        .units
        .iter()
        .try_fold(0usize, |total, unit| total.checked_add(unit.content.len()))
        .and_then(|total| total.checked_add(native.units.len().saturating_sub(1) * 2));
    let Some(mut text_bytes) =
        text_bytes.filter(|total| (1..=MAX_TEXT).contains(&text_limit) && *total <= text_limit)
    else {
        return Err(FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::ContentInvalid,
            message: "native text exceeds the admitted extraction text budget".into(),
        });
    };
    if native.visual_inputs.is_empty() {
        return Ok(VisualRecognitionReceipt::default());
    }
    let mut ordinals = std::collections::HashSet::new();
    let mut page_kinds = std::collections::HashMap::new();
    let mut decoded = Vec::with_capacity(native.visual_inputs.len());
    for visual in &native.visual_inputs {
        let bytes =
            STANDARD.decode(&visual.content_base64).map_err(|_| invalid_visual(file_kind))?;
        if !ordinals.insert(visual.ordinal)
            || visual.width == 0
            || visual.height == 0
            || hex::encode(Sha256::digest(&bytes)) != visual.payload_sha256
            || !native.units.iter().any(|unit| unit.ordinal == visual.source_unit)
        {
            return Err(invalid_visual(file_kind));
        }
        let kinds = page_kinds.entry(visual.source_unit).or_insert((false, false));
        match visual.kind {
            VisualInputKind::PdfPage => {
                if kinds.0 {
                    return Err(invalid_visual(file_kind));
                }
                kinds.0 = true;
            }
            VisualInputKind::EmbeddedPicture => kinds.1 = true,
            VisualInputKind::RasterFrame => {}
        }
        if kinds.0 && kinds.1 {
            return Err(invalid_visual(file_kind));
        }
        decoded.push(bytes);
    }
    let provider =
        input.vision_provider.ok_or_else(|| super::missing_vision_binding_error(file_kind))?;
    let empty_parameters = serde_json::json!({});
    let parameters = input.extra_parameters_json.unwrap_or(&empty_parameters);
    let binding_identity = serde_json::json!({
        "provider":provider.provider_kind,"model":provider.model_name,
        "parameters_sha256":hex::encode(Sha256::digest(serde_json::to_vec(parameters).map_err(|_| invalid_visual(file_kind))?)),
        "endpoint_sha256":hex::encode(Sha256::digest(input.base_url.unwrap_or_default().as_bytes())),
    });
    let mut receipt = VisualRecognitionReceipt::default();
    let mut recognized = Vec::new();
    let mut text_units = native
        .units
        .iter()
        .filter(|unit| !unit.content.trim().is_empty())
        .map(|unit| unit.ordinal)
        .collect::<std::collections::HashSet<_>>();
    for (visual, bytes) in native.visual_inputs.iter().zip(decoded) {
        let result = image::recognize_visual(
            input.gateway,
            image::VisionProviderInput {
                provider_kind: &provider.provider_kind,
                model_name: &provider.model_name,
                api_key: input.api_key,
                base_url: input.base_url,
                extra_parameters_json: parameters,
            },
            &visual.mime,
            &bytes,
            input.cancellation_token,
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                receipt.calls.extend(error.calls);
                return Err(FileExtractError::VisionFailed {
                    file_kind,
                    class: ExtractionFailureClass::OperationalRetryable,
                    message: error.message.into(),
                    calls: receipt.calls,
                });
            }
        };
        receipt.calls.extend(result.calls);
        receipt.warnings.extend(result.warnings);
        if let VisualTextOutcome::Text { text } = &result.outcome {
            let separator_bytes = if text_units.insert(visual.source_unit) { 0 } else { 2 };
            text_bytes =
                text_bytes.saturating_add(separator_bytes).saturating_add(text.trim().len());
            if text_bytes > text_limit {
                return Err(FileExtractError::VisionFailed {
                    file_kind,
                    class: ExtractionFailureClass::ContentInvalid,
                    message: "recognized text exceeds the extraction text budget".into(),
                    calls: receipt.calls,
                });
            }
        }
        receipt.outcomes.push(serde_json::json!({
            "kind":visual.kind,"ordinal":visual.ordinal,"native_index":visual.native_index,
            "source_unit":visual.source_unit,"source_path":visual.source_path,
            "payload_sha256":visual.payload_sha256,"mime":visual.mime,
            "width":visual.width,"height":visual.height,
            "display_extent_96_dpi":visual.display_extent_96_dpi,
            "binding":binding_identity,
            "prompt_sha256":hex::encode(Sha256::digest(image::IMAGE_OCR_PROMPT.as_bytes())),
            "outcome":result.outcome,
        }));
        recognized.push((visual.source_unit, result.outcome));
    }
    for (ordinal, outcome) in recognized {
        if let VisualTextOutcome::Text { text } = outcome {
            let unit = native
                .units
                .iter_mut()
                .find(|unit| unit.ordinal == ordinal)
                .ok_or_else(|| invalid_visual(file_kind))?;
            if !unit.content.trim().is_empty() {
                unit.content.push_str("\n\n");
            }
            unit.content.push_str(text.trim());
        }
    }
    native.content =
        native.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    Ok(receipt)
}
