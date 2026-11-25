use std::io::Cursor;

use anyhow::{Context, Result, anyhow};
use image::{
    DynamicImage, ImageBuffer, ImageDecoder, ImageFormat, Luma, LumaA, Rgb, Rgba,
    imageops::FilterType,
};
use png::{
    ColorType as PngColorType, Decoder as PngDecoder, Transformations as PngTransformations,
};

use crate::{
    integrations::llm::{LlmGateway, VisionRequest, disable_bounded_answer_reasoning},
    shared::extraction::{
        ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content, text_quality,
    },
};

pub const IMAGE_OCR_PROMPT: &str = "Transcribe every visible word verbatim, preserving its writing system and line breaks. Return exactly one JSON object: {\"outcome\":\"text\",\"text\":\"visible text\"} for readable text, or {\"outcome\":\"no_text\"} for a blank or decorative image with no readable text. When the image is a diagram whose labels and connectors are clearly legible, add \"relations\":[{\"subject\":\"label\",\"relation\":\"connector label\",\"object\":\"label\",\"direction\":\"optional connector direction\"}] containing only relations you can actually read; omit every relation whose labels, connector or direction is unclear, and never invent names, tables or relationships. Do not describe the image, do not transliterate, and do not wrap the JSON in markdown.";
const IMAGE_DESCRIPTION_PROMPT: &str = "Describe this image in detail, including any text, data, tables, diagrams, charts, formulas, signage, and other visually meaningful content that is visible.";

pub struct ImageVisionOutput {
    pub text: String,
    pub warnings: Vec<String>,
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: serde_json::Value,
    pub mime_type: String,
}

/// Connection parameters for one vision provider call.
///
/// The provider identity, credential overrides, and request extras travel
/// together; grouping them keeps the image bytes and MIME type from being
/// interleaved with five positional provider fields.
#[derive(Clone, Copy)]
pub struct VisionProviderInput<'a> {
    pub dispatch: Option<&'a crate::shared::provider_http::DispatchAuthority>,
    pub max_output_tokens_override: Option<i32>,
    pub provider_kind: &'a str,
    pub model_name: &'a str,
    pub api_key: &'a str,
    pub base_url: Option<&'a str>,
    pub extra_parameters_json: &'a serde_json::Value,
}

/// Digest identity of the paid Vision binding: provider, model, parameter and
/// endpoint digests. Credentials are never part of it.
#[must_use]
pub fn vision_binding_identity(input: &VisionProviderInput<'_>) -> serde_json::Value {
    serde_json::json!({
        "provider": input.provider_kind,
        "model": input.model_name,
        "parameters_sha256": hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
            input.extra_parameters_json.to_string().as_bytes()
        )),
        "endpoint_sha256": hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
            input.base_url.unwrap_or_default().as_bytes()
        )),
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum VisualTextOutcome {
    Text { text: String },
    NoText,
}

impl VisualTextOutcome {
    /// Whether this reading may supply graph relation evidence.
    ///
    /// Only verbatim transcription of readable text that passes the shared
    /// native-text quality assessment qualifies; low-confidence OCR text is
    /// never promoted to a table name, foreign key or relation direction.
    #[must_use]
    pub fn admits_relation_evidence(&self) -> bool {
        match self {
            Self::Text { text } => text_quality::is_graph_extraction_text_eligible(text),
            Self::NoText => false,
        }
    }
}

/// Evidence class of a visual reading recorded in the extraction source map.
/// Ingest Vision only transcribes visible text; interpreted structure is a
/// separate, inferred record with its own provenance.
pub const VISUAL_TRANSCRIPTION_EVIDENCE: &str = "verbatim_transcription";

/// Marker for a unit whose native text is low-confidence and that received no
/// visual reading, so nothing verified its semantics.
pub const LOW_CONFIDENCE_WITHOUT_VISUAL_READING: &str =
    "low_confidence_native_text_without_visual_reading";

/// How one source unit's text was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualCoverage {
    /// No native text and no visual reading produced text.
    None,
    /// The unit kept its own native text layer.
    NativeText,
    /// The whole page or frame was rendered and read by Vision.
    FullPageVision,
    /// Only an embedded picture of the unit was read by Vision.
    EmbeddedPictureVision,
}

/// One interpreted relation of a diagram.
///
/// A relation alone is never evidence: it is only valid inside
/// [`InferredVisualStructure`], which carries the prompt hash and binding
/// identity a consumer must keep.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredVisualRelation {
    pub subject: String,
    pub relation: String,
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

/// An explicit interpretation of one visual input's structure.
///
/// `inferred` is always `true`. Interpreted structure never joins the source
/// text and never becomes verified relation evidence by itself. Ingest Vision
/// only transcribes, so an ordinary document carries an empty list here and
/// the transcription/interpretation distinction stays representable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InferredVisualStructure {
    pub inferred: bool,
    pub source_unit: u32,
    pub prompt_sha256: String,
    pub binding: serde_json::Value,
    pub relations: Vec<InferredVisualRelation>,
}

impl InferredVisualStructure {
    /// Builds an inferred record from the interpreting prompt and the paid
    /// binding identity that produced it.
    #[must_use]
    pub fn new(
        source_unit: u32,
        prompt: &str,
        binding: serde_json::Value,
        relations: Vec<InferredVisualRelation>,
    ) -> Self {
        Self {
            inferred: true,
            source_unit,
            prompt_sha256: prompt_sha256(prompt),
            binding,
            relations,
        }
    }

    /// Interpreted structure is never appended to source text.
    #[must_use]
    pub const fn contributes_source_text(&self) -> bool {
        false
    }
}

/// SHA-256 of a recognition prompt; the prompt identity of a visual record.
#[must_use]
pub fn prompt_sha256(prompt: &str) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(prompt.as_bytes()))
}

/// Observable per-unit quality and coverage record of one extraction unit.
///
/// `text_quality` describes the delivered unit text; the `native_*` fields and
/// `visual_coverage` describe how it was obtained. When the native layer is
/// low-confidence and no visual reading was performed, `evidence_limitation`
/// says the text is not verified evidence of names or relations.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UnitEvidence {
    pub ordinal: u32,
    pub text_score: f32,
    pub text_low_confidence: bool,
    pub text_reasons: Vec<&'static str>,
    pub native_text_low_confidence: bool,
    pub visual_coverage: VisualCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_limitation: Option<&'static str>,
}

impl UnitEvidence {
    /// Evidence record for one unit: delivered text, how the unit was covered,
    /// and whether any visual reading produced text for it.
    #[must_use]
    pub fn new(
        ordinal: u32,
        text: &str,
        native_text_low_confidence: bool,
        coverage: VisualCoverage,
        visual_read_text: bool,
    ) -> Self {
        let quality = text_quality::assess_text_quality(text);
        // A reading that produced no text left the unit unverified, so the
        // limitation is truthful even when a visual input was admitted.
        let evidence_limitation = (native_text_low_confidence && !visual_read_text)
            .then_some(LOW_CONFIDENCE_WITHOUT_VISUAL_READING);
        Self {
            ordinal,
            text_score: quality.score,
            text_low_confidence: quality.low_confidence,
            text_reasons: quality.reasons,
            native_text_low_confidence,
            visual_coverage: coverage,
            evidence_limitation,
        }
    }

    /// Coverage of a unit that kept its own native text layer.
    #[must_use]
    pub fn native_text_only(ordinal: u32, text: &str) -> Self {
        let native_text_low_confidence = text_quality::is_low_confidence_text(text);
        let coverage =
            if text.trim().is_empty() { VisualCoverage::None } else { VisualCoverage::NativeText };
        Self::new(ordinal, text, native_text_low_confidence, coverage, false)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VisionCallReceipt {
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: serde_json::Value,
}

#[derive(Debug)]
pub struct VisionRecognitionError {
    pub message: &'static str,
    pub calls: Vec<VisionCallReceipt>,
    pub terminal: bool,
    /// Reserved billing provider-call row of the failed physical attempt, set
    /// only when this attempt's reservation had already succeeded. A caller
    /// must never substitute an ID when this is `None`.
    pub provider_call_id: Option<uuid::Uuid>,
}

fn terminal_vision_error(error: &anyhow::Error) -> bool {
    use crate::integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError};
    error.downcast_ref::<ProviderCallError>().is_some_and(|error| {
        error.is_permanent_rejection()
            || matches!(
                error,
                ProviderCallError::Admission { source: ProviderBudgetError::Dispatch(_), .. }
            )
    })
}

impl std::fmt::Display for VisionRecognitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for VisionRecognitionError {}

pub struct RecognizedVisual {
    pub outcome: VisualTextOutcome,
    /// Interpreted diagram relations. Kept apart from `outcome`: they are never
    /// part of the transcribed text.
    pub relations: Vec<InferredVisualRelation>,
    pub calls: Vec<VisionCallReceipt>,
    pub warnings: Vec<String>,
    pub mime_type: String,
    /// Reserved and settled provider-call row of the deciding attempt, when a
    /// ledger was supplied. `None` means no reservation was made.
    pub provider_call_id: Option<uuid::Uuid>,
}

/// Reservation key of one physical paid Vision attempt.
pub struct VisionLedgerRequest<'a> {
    /// 1-based physical attempt inside this recognition request.
    pub attempt: u8,
    pub provider_kind: &'a str,
    pub model_name: &'a str,
    pub mime_type: &'a str,
}

/// Durable reservation owner of paid Vision calls.
///
/// The ingestion boundary owns the ledger and supplies the real ingest attempt,
/// binding and provider/model catalog identities. This module only enforces the
/// order reserve -> request -> settle, and fails the reserved row when the
/// request ends in a terminal error. Nothing here synthesizes an identity.
#[async_trait::async_trait]
pub trait VisionCallLedger: Send + Sync {
    /// Reserves one existing billing provider-call row before the paid request.
    async fn reserve(
        &self,
        request: VisionLedgerRequest<'_>,
    ) -> std::result::Result<(uuid::Uuid, crate::shared::provider_send::ProviderSendScope), String>;
    /// Settles the reserved row with the provider's reported usage.
    async fn settle(
        &self,
        provider_call_id: uuid::Uuid,
        usage_json: &serde_json::Value,
    ) -> std::result::Result<(), String>;
    /// Marks the reserved row failed after a terminal request error.
    async fn fail(&self, provider_call_id: uuid::Uuid) -> std::result::Result<(), String>;
}

/// Provider outcome envelope: verbatim transcription plus optional interpreted
/// relations. Unknown relation keys are tolerated so one extra provider field
/// never destroys a valid transcription.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum VisualRecognitionResponse {
    Text {
        text: String,
        #[serde(default)]
        relations: Vec<InferredVisualRelation>,
    },
    NoText,
}

/// Drops structurally empty relations: an invented or unread relation has no
/// readable subject, predicate or object and therefore no evidence value.
fn retained_relations(relations: Vec<InferredVisualRelation>) -> Vec<InferredVisualRelation> {
    relations
        .into_iter()
        .filter(|relation| {
            !relation.subject.trim().is_empty()
                && !relation.relation.trim().is_empty()
                && !relation.object.trim().is_empty()
        })
        .collect()
}

/// Parses the provider outcome into the verbatim transcription and any
/// explicitly interpreted relations. Empty transcription text stays invalid.
fn parse_visual_response(text: &str) -> Option<(VisualTextOutcome, Vec<InferredVisualRelation>)> {
    let response: VisualRecognitionResponse = serde_json::from_str(text).ok()?;
    match response {
        VisualRecognitionResponse::Text { text, .. } if text.trim().is_empty() => None,
        VisualRecognitionResponse::Text { text, relations } => {
            Some((VisualTextOutcome::Text { text }, retained_relations(relations)))
        }
        VisualRecognitionResponse::NoText => Some((VisualTextOutcome::NoText, Vec::new())),
    }
}

async fn fail_reservation(ledger: Option<&dyn VisionCallLedger>, provider_call_id: uuid::Uuid) {
    let Some(ledger) = ledger else {
        return;
    };
    if let Err(error) = ledger.fail(provider_call_id).await {
        tracing::error!(%provider_call_id, %error, "vision reservation could not be failed");
    }
}

/// Runs one Vision request with the reservation's send scope installed.
///
/// The admission owner refuses a paid send that has no ambient attribution, so
/// the scope must wrap the request itself, not only the settlement.
async fn run_vision_under_reservation(
    reservation: Option<&(uuid::Uuid, crate::shared::provider_send::ProviderSendScope)>,
    gateway: &dyn LlmGateway,
    input: VisionProviderInput<'_>,
    mime_type: &str,
    file_bytes: &[u8],
    prompt: &str,
) -> Result<ImageVisionOutput> {
    match reservation {
        Some((_, scope)) => {
            crate::shared::provider_send::with_provider_send(
                scope.clone(),
                run_vision_image_request(gateway, input, mime_type, file_bytes, prompt),
            )
            .await
        }
        None => run_vision_image_request(gateway, input, mime_type, file_bytes, prompt).await,
    }
}

/// Settles the paid Vision call. The response is already paid, so accounting
/// failure is reported and never discards it.
async fn settle_vision_call(
    ledger: Option<&dyn VisionCallLedger>,
    reservation: Option<&(uuid::Uuid, crate::shared::provider_send::ProviderSendScope)>,
    usage_json: &serde_json::Value,
) {
    let Some((provider_call_id, _)) = reservation else {
        return;
    };
    let Some(ledger) = ledger else {
        return;
    };
    if let Err(error) = ledger.settle(*provider_call_id, usage_json).await {
        tracing::error!(%provider_call_id, %error, "vision usage settlement failed after a paid response");
    }
}

/// Empty responses alone receive one retry. Every completed call keeps its raw usage.
pub async fn recognize_visual_with_ledger(
    gateway: &dyn LlmGateway,
    input: VisionProviderInput<'_>,
    mime_type: &str,
    file_bytes: &[u8],
    cancellation_token: Option<&tokio_util::sync::CancellationToken>,
    ledger: Option<&dyn VisionCallLedger>,
) -> std::result::Result<RecognizedVisual, VisionRecognitionError> {
    let mut calls = Vec::new();
    for attempt in 0..2 {
        if cancellation_token.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Err(VisionRecognitionError {
                message: "visual recognition canceled",
                calls,
                terminal: false,
                provider_call_id: None,
            });
        }
        let reservation = match ledger {
            Some(ledger) => {
                let request = VisionLedgerRequest {
                    attempt: attempt + 1,
                    provider_kind: input.provider_kind,
                    model_name: input.model_name,
                    mime_type,
                };
                match ledger.reserve(request).await {
                    Ok(reserved) => Some(reserved),
                    Err(_) => {
                        // Financial admission is fail-closed: no paid request is
                        // issued without a durable reservation.
                        return Err(VisionRecognitionError {
                            message: "Vision billing reservation failed; the paid request was not issued",
                            calls,
                            terminal: true,
                            provider_call_id: None,
                        });
                    }
                }
            }
            None => None,
        };
        let response = match run_vision_under_reservation(
            reservation.as_ref(),
            gateway,
            input,
            mime_type,
            file_bytes,
            IMAGE_OCR_PROMPT,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                if let Some((provider_call_id, _)) = reservation.as_ref() {
                    fail_reservation(ledger, *provider_call_id).await;
                }
                return Err(VisionRecognitionError {
                    // Provider errors may contain credentials or response bodies.
                    message: "Vision request failed; check the extract_text binding and provider diagnostics",
                    calls: calls.clone(),
                    terminal: terminal_vision_error(&error),
                    provider_call_id: reservation.as_ref().map(|(id, _)| *id),
                });
            }
        };
        settle_vision_call(ledger, reservation.as_ref(), &response.usage_json).await;
        calls.push(VisionCallReceipt {
            provider_kind: response.provider_kind,
            model_name: response.model_name,
            usage_json: response.usage_json,
        });
        if response.text.trim().is_empty() && attempt == 0 {
            continue;
        }
        let (outcome, relations) =
            parse_visual_response(&response.text).ok_or_else(|| VisionRecognitionError {
                message: "Vision returned an empty or invalid recognition outcome",
                calls: calls.clone(),
                terminal: false,
                provider_call_id: reservation.as_ref().map(|(id, _)| *id),
            })?;
        return Ok(RecognizedVisual {
            outcome,
            relations,
            calls,
            warnings: response.warnings,
            mime_type: response.mime_type,
            provider_call_id: reservation.as_ref().map(|(id, _)| *id),
        });
    }
    unreachable!("the second attempt always returns an outcome or error")
}

/// Standalone image extraction with a durable reservation owner for its paid
/// Vision attempt.
pub async fn extract_image_with_provider_with_ledger(
    gateway: &dyn LlmGateway,
    input: VisionProviderInput<'_>,
    mime_type: &str,
    file_bytes: &[u8],
    cancellation_token: Option<&tokio_util::sync::CancellationToken>,
    ledger: Option<&dyn VisionCallLedger>,
) -> std::result::Result<ExtractionOutput, VisionRecognitionError> {
    let response = recognize_visual_with_ledger(
        gateway,
        input,
        mime_type,
        file_bytes,
        cancellation_token,
        ledger,
    )
    .await?;
    let text = match &response.outcome {
        VisualTextOutcome::Text { text } => text.as_str(),
        VisualTextOutcome::NoText => "",
    };
    let read_text = matches!(response.outcome, VisualTextOutcome::Text { .. });
    let inferred = if response.relations.is_empty() {
        Vec::new()
    } else {
        vec![InferredVisualStructure::new(
            1,
            IMAGE_OCR_PROMPT,
            vision_binding_identity(&input),
            response.relations,
        )]
    };
    let layout = build_text_layout_from_content(text);

    Ok(ExtractionOutput {
        extraction_kind: "vision_image".into(),
        content_text: layout.content_text,
        page_count: Some(1),
        warnings: response.warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "image".to_string(),
            page_count: Some(1),
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "mime_type": response.mime_type,
            "visual_recognition": [{"kind":"raster_frame", "source_unit":1,
                "ordinal":0,"native_index":1,"source_path":null,
                "payload_sha256":hex::encode(<sha2::Sha256 as sha2::Digest>::digest(file_bytes)),
                "prompt_sha256":hex::encode(<sha2::Sha256 as sha2::Digest>::digest(IMAGE_OCR_PROMPT.as_bytes())),
                "evidence":VISUAL_TRANSCRIPTION_EVIDENCE,"inferred":false,
                "outcome":response.outcome}],
            "unit_evidence": [
                UnitEvidence::new(1, text, false, VisualCoverage::FullPageVision, read_text),
            ],
            "inferred_visual_structure": inferred,
        }),
        provider_kind: Some(input.provider_kind.to_owned()),
        model_name: Some(input.model_name.to_owned()),
        usage_json: serde_json::json!({"vision_calls":response.calls}),
        extracted_images: Vec::new(),
    })
}

pub async fn describe_image_with_provider(
    gateway: &dyn LlmGateway,
    input: VisionProviderInput<'_>,
    mime_type: &str,
    file_bytes: &[u8],
) -> Result<ImageVisionOutput> {
    run_vision_image_request(gateway, input, mime_type, file_bytes, IMAGE_DESCRIPTION_PROMPT).await
}

async fn run_vision_image_request(
    gateway: &dyn LlmGateway,
    input: VisionProviderInput<'_>,
    mime_type: &str,
    file_bytes: &[u8],
    prompt: &str,
) -> Result<ImageVisionOutput> {
    let VisionProviderInput {
        provider_kind,
        model_name,
        api_key,
        base_url,
        extra_parameters_json,
        dispatch,
        max_output_tokens_override,
    } = input;
    let normalized_payload = prepare_vision_image_payload(file_bytes, mime_type).context(
        "image payload could not be decoded and normalized for vision extraction; re-export as PNG/JPEG and retry",
    )?;
    let request_image_bytes = normalized_payload.image_bytes;
    let request_mime_type = normalized_payload.mime_type;
    let warnings = normalized_payload.warnings;

    // Bounded extraction wants the answer, not a chain of thought: a
    // provider whose endpoint accepts the formal control (DeepSeek) gets
    // hidden reasoning switched off here, which also avoids the empty-content
    // mode its default-on thinking can land in for OCR prompts.
    let mut bounded_extra_parameters = extra_parameters_json.clone();
    disable_bounded_answer_reasoning(provider_kind, &mut bounded_extra_parameters);

    let response = gateway
        .vision_extract(VisionRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: prompt.to_string(),
            image_bytes: request_image_bytes,
            mime_type: request_mime_type.clone(),
            api_key_override: Some(api_key.to_string()),
            base_url_override: base_url.map(str::to_string),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override,
            extra_parameters_json: bounded_extra_parameters,
            dispatch: dispatch.cloned(),
        })
        .await?;

    Ok(ImageVisionOutput {
        text: response.output_text,
        warnings,
        provider_kind: response.provider_kind,
        model_name: response.model_name,
        usage_json: response.usage_json,
        mime_type: request_mime_type,
    })
}

struct PreparedVisionPayload {
    image_bytes: Vec<u8>,
    mime_type: String,
    warnings: Vec<String>,
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Whether the bytes carry a raster-image signature the extraction stack can
/// decode. Reuses the decoder crate's own magic scan instead of a second
/// signature table, and accepts only supported upload formats so an unknown
/// binary is not routed to a decoder that cannot open it.
#[must_use]
pub(crate) fn bytes_look_like_raster_image(file_bytes: &[u8]) -> bool {
    matches!(
        image::guess_format(file_bytes),
        Ok(ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::WebP
            | ImageFormat::Tiff)
    )
}

fn prepare_vision_image_payload(
    file_bytes: &[u8],
    mime_type: &str,
) -> Result<PreparedVisionPayload> {
    let mut image =
        load_normalizable_image(file_bytes, mime_type).context("failed to decode image bytes")?;
    let mut warnings = Vec::new();

    let width = image.width();
    let height = image.height();
    const MIN_DIMENSION: u32 = 64;
    if width >= MIN_DIMENSION
        && height >= MIN_DIMENSION
        && retain_opaque_jpeg(file_bytes, mime_type)?
    {
        return Ok(PreparedVisionPayload {
            image_bytes: file_bytes.to_vec(),
            mime_type: "image/jpeg".to_string(),
            warnings,
        });
    }
    if width < MIN_DIMENSION || height < MIN_DIMENSION {
        let target_width = width.max(MIN_DIMENSION);
        let target_height = height.max(MIN_DIMENSION);
        image = image.resize_exact(target_width, target_height, FilterType::Triangle);
        warnings.push(format!(
            "upscaled image from {width}x{height} to {target_width}x{target_height} for provider compatibility"
        ));
    }

    // Normalize to opaque RGB on a white matte so transparent assets do not become
    // black canvases when alpha is discarded before provider extraction.
    let image = DynamicImage::ImageRgb8(flatten_to_white_rgb8(&image));
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, ImageFormat::Png)
        .context("failed to encode normalized png payload")?;

    if !mime_type.eq_ignore_ascii_case("image/png") {
        warnings.push(format!(
            "normalized image payload from {mime_type} to image/png for provider compatibility"
        ));
    }

    Ok(PreparedVisionPayload {
        image_bytes: cursor.into_inner(),
        mime_type: "image/png".to_string(),
        warnings,
    })
}

// The preceding decode validates the entire image. Do not retain JPEGs with
// decoder-exposed EXIF, ICC, XMP or IPTC metadata: a provider could apply
// orientation or color information the decode-and-flatten path discarded.
fn retain_opaque_jpeg(bytes: &[u8], mime_type: &str) -> Result<bool> {
    if !mime_type.eq_ignore_ascii_case("image/jpeg")
        || !matches!(image::guess_format(bytes), Ok(ImageFormat::Jpeg))
    {
        return Ok(false);
    }
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))?;
    Ok(decoder.color_type() == image::ColorType::Rgb8
        && decoder.exif_metadata()?.is_none()
        && decoder.icc_profile()?.is_none()
        && decoder.xmp_metadata()?.is_none()
        && decoder.iptc_metadata()?.is_none())
}

fn load_normalizable_image(file_bytes: &[u8], mime_type: &str) -> Result<DynamicImage> {
    if mime_type.eq_ignore_ascii_case("image/png") || file_bytes.starts_with(PNG_SIGNATURE) {
        return decode_png_ignoring_checksums(file_bytes);
    }

    image::load_from_memory(file_bytes).context("failed to decode image bytes with generic decoder")
}

fn flatten_to_white_rgb8(image: &DynamicImage) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgb = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(width, height);

    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = f32::from(pixel[3]) / 255.0;
        let composite = |channel: u8| -> u8 {
            let source = f32::from(channel);
            let white = 255.0;
            source.mul_add(alpha, white * (1.0 - alpha)).round() as u8
        };
        rgb.put_pixel(x, y, Rgb([composite(pixel[0]), composite(pixel[1]), composite(pixel[2])]));
    }

    rgb
}

fn decode_png_ignoring_checksums(file_bytes: &[u8]) -> Result<DynamicImage> {
    let mut decoder = PngDecoder::new(Cursor::new(file_bytes));
    decoder.ignore_checksums(true);
    decoder.set_transformations(PngTransformations::normalize_to_color8());

    let mut reader = decoder.read_info().context("failed to read png metadata")?;
    let buffer_size = reader
        .output_buffer_size()
        .context("png decoder could not determine output buffer size")?;
    let mut buffer = vec![0; buffer_size];
    let output = reader.next_frame(&mut buffer).context("failed to decode png frame")?;
    let pixels = buffer[..output.buffer_size()].to_vec();

    match output.color_type {
        PngColorType::Grayscale => {
            ImageBuffer::<Luma<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| anyhow!("failed to construct luma image from decoded png buffer"))
        }
        PngColorType::GrayscaleAlpha => {
            ImageBuffer::<LumaA<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageLumaA8)
                .ok_or_else(|| {
                    anyhow!("failed to construct luma-alpha image from decoded png buffer")
                })
        }
        PngColorType::Rgb => {
            ImageBuffer::<Rgb<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageRgb8)
                .ok_or_else(|| anyhow!("failed to construct rgb image from decoded png buffer"))
        }
        PngColorType::Rgba => {
            ImageBuffer::<Rgba<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageRgba8)
                .ok_or_else(|| anyhow!("failed to construct rgba image from decoded png buffer"))
        }
        PngColorType::Indexed => {
            Err(anyhow!("png decoder returned indexed output after normalization"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use anyhow::Result;
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::{DynamicImage, ImageFormat};

    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionResponse,
    };

    #[derive(Default)]
    struct FakeGateway {
        last_extra_parameters: std::sync::Mutex<Option<serde_json::Value>>,
    }

    impl FakeGateway {
        fn last_extra_parameters(&self) -> serde_json::Value {
            self.last_extra_parameters
                .lock()
                .expect("fake gateway mutex")
                .clone()
                .unwrap_or_default()
        }
    }

    fn valid_png_bytes() -> Vec<u8> {
        let image = DynamicImage::new_rgba8(2, 2);
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).expect("encode generated png fixture");
        cursor.into_inner()
    }

    fn synthetic_jpeg(width: u32, height: u32, exif: Option<Vec<u8>>) -> Vec<u8> {
        use image::ImageEncoder;
        let image = ImageBuffer::from_fn(width, height, |x, y| Rgb([x as u8, y as u8, 42]));
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut bytes);
        if let Some(exif) = exif {
            encoder.set_exif_metadata(exif).expect("set EXIF");
        }
        encoder
            .write_image(image.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .expect("encode JPEG");
        bytes
    }

    #[test]
    fn jpeg_retention_preserves_decoder_pixels_and_validates_format() {
        let jpeg = synthetic_jpeg(80, 72, None);
        let old_pixels =
            flatten_to_white_rgb8(&load_normalizable_image(&jpeg, "image/jpeg").unwrap());
        let payload = prepare_vision_image_payload(&jpeg, "image/jpeg").unwrap();
        assert_eq!(payload.mime_type, "image/jpeg");
        assert_eq!(payload.image_bytes, jpeg);
        assert_eq!(image::load_from_memory(&payload.image_bytes).unwrap().to_rgb8(), old_pixels);
        let wrong_mime = prepare_vision_image_payload(&jpeg, "image/webp").unwrap();
        assert_eq!(wrong_mime.mime_type, "image/png");
        assert!(prepare_vision_image_payload(&jpeg[..jpeg.len() / 2], "image/jpeg").is_err());
        assert!(prepare_vision_image_payload(b"not an image", "image/jpeg").is_err());
    }

    #[test]
    fn jpeg_requiring_resize_or_exif_removal_is_normalized() {
        let tiny = synthetic_jpeg(10, 20, None);
        let payload = prepare_vision_image_payload(&tiny, "image/jpeg").unwrap();
        assert_eq!(payload.mime_type, "image/png");
        assert_eq!(
            (
                image::load_from_memory(&payload.image_bytes).unwrap().width(),
                image::load_from_memory(&payload.image_bytes).unwrap().height()
            ),
            (64, 64)
        );
        // Little-endian TIFF with orientation 6 (rotate 90 degrees).
        let exif = vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let jpeg = synthetic_jpeg(80, 72, Some(exif));
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&jpeg)).unwrap();
        assert_ne!(decoder.orientation().unwrap(), image::metadata::Orientation::NoTransforms);
        let old = flatten_to_white_rgb8(&load_normalizable_image(&jpeg, "image/jpeg").unwrap());
        let payload = prepare_vision_image_payload(&jpeg, "image/jpeg").unwrap();
        assert_eq!(payload.mime_type, "image/png");
        assert_eq!(image::load_from_memory(&payload.image_bytes).unwrap().to_rgb8(), old);
    }

    fn tiny_grayscale_alpha_png_bytes() -> Vec<u8> {
        STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aF9sAAAAASUVORK5CYII=")
            .expect("decode tiny png fixture")
    }

    fn transparent_black_png_bytes() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([0, 0, 0, 0])));
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).expect("encode transparent png fixture");
        cursor.into_inner()
    }

    #[async_trait]
    impl LlmGateway for FakeGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            unreachable!("generate is not used in image extraction tests")
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in image extraction tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in image extraction tests")
        }

        async fn vision_extract(&self, mut request: VisionRequest) -> Result<VisionResponse> {
            *self.last_extra_parameters.lock().expect("fake gateway mutex") =
                Some(request.extra_parameters_json.clone());
            let mime_type = request.mime_type.clone();
            Ok(VisionResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
                output_text: if request.prompt == IMAGE_OCR_PROMPT {
                    serde_json::json!({"outcome":"text","text":format!("diagram text and entities [{mime_type}]")}).to_string()
                } else {
                    format!("diagram text and entities [{mime_type}]")
                },
                usage_json: serde_json::json!({}),
            })
        }
    }

    #[tokio::test]
    async fn normalizes_provider_vision_response() {
        let output = extract_image_with_provider_with_ledger(
            &FakeGateway::default(),
            VisionProviderInput {
                max_output_tokens_override: None,
                dispatch: None,
                provider_kind: "openai",
                model_name: "gpt-5.4-mini",
                api_key: "test-key",
                base_url: None,
                extra_parameters_json: &serde_json::json!({
                    "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
                }),
            },
            "image/png",
            &valid_png_bytes(),
            None,
            None,
        )
        .await
        .expect("image extraction");

        assert_eq!(output.extraction_kind, "vision_image");
        assert_eq!(output.page_count, Some(1));
        assert_eq!(output.provider_kind.as_deref(), Some("openai"));
        assert_eq!(output.model_name.as_deref(), Some("gpt-5.4-mini"));
        assert!(output.content_text.contains("diagram text"));
        assert_eq!(
            output.source_map["unit_evidence"][0]["visual_coverage"],
            serde_json::json!("full_page_vision")
        );
        assert_eq!(
            output.source_map["visual_recognition"][0]["evidence"],
            serde_json::json!(VISUAL_TRANSCRIPTION_EVIDENCE)
        );
        assert_eq!(
            output.source_map["visual_recognition"][0]["inferred"],
            serde_json::json!(false)
        );
        assert_eq!(output.source_map["inferred_visual_structure"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn normalizes_non_png_mime_payloads_before_provider_call() {
        let output = extract_image_with_provider_with_ledger(
            &FakeGateway::default(),
            VisionProviderInput {
                max_output_tokens_override: None,
                dispatch: None,
                provider_kind: "openai",
                model_name: "gpt-5.4-mini",
                api_key: "test-key",
                base_url: None,
                extra_parameters_json: &serde_json::json!({
                    "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
                }),
            },
            "image/webp",
            &valid_png_bytes(),
            None,
            None,
        )
        .await
        .expect("image extraction");

        assert_eq!(output.source_map["mime_type"], serde_json::json!("image/png"));
        assert!(!output.warnings.is_empty());
        assert!(output.content_text.contains("[image/png]"));
    }

    #[tokio::test]
    async fn source_image_description_reuses_the_normalized_png_payload() {
        let output = describe_image_with_provider(
            &FakeGateway::default(),
            VisionProviderInput {
                max_output_tokens_override: None,
                dispatch: None,
                provider_kind: "openai",
                model_name: "gpt-5.4-mini",
                api_key: "test-key",
                base_url: None,
                extra_parameters_json: &serde_json::json!({
                    "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
                }),
            },
            "image/webp",
            &valid_png_bytes(),
        )
        .await
        .expect("image description");

        assert_eq!(output.provider_kind, "openai");
        assert_eq!(output.model_name, "gpt-5.4-mini");
        assert_eq!(output.mime_type, "image/png");
        assert!(output.text.contains("[image/png]"));
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("normalized image payload from image/webp"))
        );
    }

    #[test]
    fn normalizes_tiny_grayscale_alpha_png_payloads() {
        let payload = prepare_vision_image_payload(&tiny_grayscale_alpha_png_bytes(), "image/png")
            .expect("normalize tiny grayscale alpha png");

        let decoded =
            image::load_from_memory(&payload.image_bytes).expect("decode normalized payload");

        assert_eq!(payload.mime_type, "image/png");
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
        assert_eq!(decoded.color(), image::ColorType::Rgb8);
        assert!(
            payload
                .warnings
                .iter()
                .any(|warning| warning.contains("upscaled image from 1x1 to 64x64"))
        );
    }

    #[test]
    fn transparent_images_are_matted_to_white_before_alpha_drop() {
        let payload = prepare_vision_image_payload(&transparent_black_png_bytes(), "image/png")
            .expect("normalize transparent png");

        let decoded = image::load_from_memory(&payload.image_bytes)
            .expect("decode transparent normalized payload")
            .to_rgb8();

        for pixel in decoded.pixels() {
            assert_eq!(pixel.0, [255, 255, 255]);
        }
    }

    #[tokio::test]
    async fn bounded_extraction_disables_hidden_reasoning_when_the_profile_declares_it() {
        let gateway = FakeGateway::default();
        let declaring = serde_json::json!({
            "_providerProfile": {
                "runtime": {"kind": "openai_compatible"},
                "requestPolicy": {"reasoning": "disable_with_none"}
            }
        });
        extract_image_with_provider_with_ledger(
            &gateway,
            VisionProviderInput {
                max_output_tokens_override: None,
                dispatch: None,
                provider_kind: "deepseek",
                model_name: "deepseek-flash",
                api_key: "test-key",
                base_url: None,
                extra_parameters_json: &declaring,
            },
            "image/png",
            &valid_png_bytes(),
            None,
            None,
        )
        .await
        .expect("image extraction");
        assert_eq!(gateway.last_extra_parameters()["reasoning_effort"], serde_json::json!("none"));

        let gateway = FakeGateway::default();
        let forwarding = serde_json::json!({
            "_providerProfile": {
                "runtime": {"kind": "openai_compatible"},
                "requestPolicy": {"reasoning": "forward"}
            }
        });
        extract_image_with_provider_with_ledger(
            &gateway,
            VisionProviderInput {
                max_output_tokens_override: None,
                dispatch: None,
                provider_kind: "openai",
                model_name: "gpt-5.4-mini",
                api_key: "test-key",
                base_url: None,
                extra_parameters_json: &forwarding,
            },
            "image/png",
            &valid_png_bytes(),
            None,
            None,
        )
        .await
        .expect("image extraction");
        assert!(gateway.last_extra_parameters().get("reasoning_effort").is_none());
    }

    /// Case/digit-turbulent token run that `text_quality` classifies as
    /// low-confidence OCR text.
    fn ocr_noise_text() -> &'static str {
        concat!(
            "summary topic alpha beta gamma ",
            "abCD4efGH hiJKlmNO pQrST uvWXyZab ",
            "cdEFGh3Ij klMNOprs tuVWxyZq mnOPqRst"
        )
    }

    #[test]
    fn transcription_admits_relation_evidence_only_for_readable_text() {
        let readable = VisualTextOutcome::Text {
            text: "Orders belong to customers; invoices reference orders.".to_string(),
        };
        assert!(readable.admits_relation_evidence());

        let noise = VisualTextOutcome::Text { text: ocr_noise_text().to_string() };
        assert!(!noise.admits_relation_evidence(), "OCR noise is not relation evidence");
        assert!(!VisualTextOutcome::NoText.admits_relation_evidence());
    }

    #[test]
    fn native_unit_evidence_marks_low_confidence_text_without_a_reading() {
        let limited = UnitEvidence::native_text_only(3, ocr_noise_text());
        assert_eq!(limited.visual_coverage, VisualCoverage::NativeText);
        assert!(limited.native_text_low_confidence);
        assert!(limited.text_low_confidence);
        assert_eq!(limited.evidence_limitation, Some(LOW_CONFIDENCE_WITHOUT_VISUAL_READING));

        let covered =
            UnitEvidence::new(3, ocr_noise_text(), true, VisualCoverage::FullPageVision, true);
        assert_eq!(covered.visual_coverage, VisualCoverage::FullPageVision);
        assert_eq!(covered.evidence_limitation, None);

        let unread =
            UnitEvidence::new(3, ocr_noise_text(), true, VisualCoverage::FullPageVision, false);
        assert_eq!(
            unread.evidence_limitation,
            Some(LOW_CONFIDENCE_WITHOUT_VISUAL_READING),
            "a reading that produced no text leaves the limitation truthful"
        );

        let readable = UnitEvidence::native_text_only(1, "The service stores project settings.");
        assert!(!readable.native_text_low_confidence);
        assert_eq!(readable.evidence_limitation, None);

        let empty = UnitEvidence::native_text_only(2, "   ");
        assert_eq!(empty.visual_coverage, VisualCoverage::None);
        assert_eq!(empty.evidence_limitation, None);
    }

    #[test]
    fn inferred_structure_stays_separate_from_verbatim_transcription() {
        let binding =
            serde_json::json!({"provider":"openai","model":"gpt-6-luna","parameters_sha256":"0"});
        let inferred = InferredVisualStructure::new(
            2,
            "interpret the diagram relations",
            binding.clone(),
            vec![InferredVisualRelation {
                subject: "orders".into(),
                relation: "references".into(),
                object: "customers".into(),
                direction: Some("orders -> customers".into()),
            }],
        );

        assert!(inferred.inferred);
        assert_eq!(inferred.source_unit, 2);
        assert_eq!(inferred.prompt_sha256, prompt_sha256("interpret the diagram relations"));
        assert_ne!(inferred.prompt_sha256, prompt_sha256(IMAGE_OCR_PROMPT));
        assert_eq!(inferred.binding, binding);
        assert!(!inferred.contributes_source_text());
        assert_eq!(inferred.relations.len(), 1);

        let serialized = serde_json::to_value(&inferred).expect("inferred record serializes");
        assert_eq!(serialized["inferred"], serde_json::json!(true));
        assert_eq!(
            serialized["promptSha256"],
            serde_json::json!(prompt_sha256("interpret the diagram relations"))
        );
        assert_eq!(serialized["binding"], binding);
    }

    fn vision_input<'a>(
        provider_kind: &'a str,
        model_name: &'a str,
        extra_parameters_json: &'a serde_json::Value,
    ) -> VisionProviderInput<'a> {
        VisionProviderInput {
            dispatch: None,
            max_output_tokens_override: None,
            provider_kind,
            model_name,
            api_key: "test-key",
            base_url: None,
            extra_parameters_json,
        }
    }

    #[derive(Default)]
    struct ScriptedVisionGateway {
        calls: std::sync::atomic::AtomicUsize,
        empty_first: bool,
        relations: bool,
        error: bool,
    }

    #[async_trait]
    impl LlmGateway for ScriptedVisionGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            unreachable!("generate is not used in vision ledger tests")
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in vision ledger tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in vision ledger tests")
        }

        async fn vision_extract(&self, mut request: VisionRequest) -> Result<VisionResponse> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.error {
                return Err(anyhow!("provider exploded"));
            }
            let output_text = if self.empty_first && call == 0 {
                String::new()
            } else if self.relations {
                serde_json::json!({
                    "outcome":"text",
                    "text":"Orders and customers are drawn as separate boxes",
                    "relations":[
                        {"subject":"orders","relation":"references","object":"customers",
                         "direction":"orders -> customers"},
                        {"subject":"","relation":"belongs to","object":"customers"}
                    ]
                })
                .to_string()
            } else {
                serde_json::json!({"outcome":"text","text":"Counted OCR text"}).to_string()
            };
            Ok(VisionResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
                output_text,
                usage_json: serde_json::json!({"total_tokens": 7}),
            })
        }
    }

    #[derive(Default)]
    struct RecordingLedger {
        attempts: std::sync::Mutex<Vec<u8>>,
        settled: std::sync::Mutex<Vec<serde_json::Value>>,
        failed: std::sync::atomic::AtomicUsize,
        refuse: bool,
        settle_refuses: bool,
    }

    fn reserved_call_id() -> uuid::Uuid {
        uuid::Uuid::from_u128(0x5eed)
    }

    #[async_trait]
    impl VisionCallLedger for RecordingLedger {
        async fn reserve(
            &self,
            request: VisionLedgerRequest<'_>,
        ) -> std::result::Result<
            (uuid::Uuid, crate::shared::provider_send::ProviderSendScope),
            String,
        > {
            if self.refuse {
                return Err("reservation refused".to_string());
            }
            self.attempts.lock().expect("ledger mutex").push(request.attempt);
            let provider_call_id = reserved_call_id();
            let attribution = crate::shared::provider_send::ProviderSendAttribution {
                workspace_id: uuid::Uuid::now_v7(),
                library_id: uuid::Uuid::now_v7(),
                owning_execution_kind: "ingest_attempt".to_string(),
                owning_execution_id: uuid::Uuid::now_v7(),
                call_kind: "vision_extract".to_string(),
                binding_id: None,
                provider_catalog_id: uuid::Uuid::now_v7(),
                model_catalog_id: uuid::Uuid::now_v7(),
                runtime_execution_id: None,
                runtime_task_kind: None,
            };
            Ok((
                provider_call_id,
                crate::shared::provider_send::ProviderSendScope::new(attribution, provider_call_id),
            ))
        }

        async fn settle(
            &self,
            _provider_call_id: uuid::Uuid,
            usage_json: &serde_json::Value,
        ) -> std::result::Result<(), String> {
            self.settled.lock().expect("ledger mutex").push(usage_json.clone());
            if self.settle_refuses {
                return Err("settlement was refused".to_string());
            }
            Ok(())
        }

        async fn fail(&self, _provider_call_id: uuid::Uuid) -> std::result::Result<(), String> {
            self.failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn vision_ledger_reserves_and_settles_each_physical_attempt() {
        let gateway = ScriptedVisionGateway { empty_first: true, ..Default::default() };
        let ledger = RecordingLedger::default();
        let parameters = serde_json::json!({});
        let recognized = recognize_visual_with_ledger(
            &gateway,
            vision_input("openai", "gpt-vision", &parameters),
            "image/png",
            &valid_png_bytes(),
            None,
            Some(&ledger),
        )
        .await
        .expect("the second attempt returns transcription");

        assert_eq!(recognized.outcome, VisualTextOutcome::Text { text: "Counted OCR text".into() });
        assert_eq!(*ledger.attempts.lock().expect("ledger mutex"), vec![1, 2]);
        assert_eq!(ledger.settled.lock().expect("ledger mutex").len(), 2);
        assert_eq!(ledger.failed.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(recognized.provider_call_id, Some(reserved_call_id()));
    }

    #[tokio::test]
    async fn vision_ledger_settlement_failure_keeps_the_observed_response() {
        let gateway = ScriptedVisionGateway::default();
        let ledger = RecordingLedger { settle_refuses: true, ..Default::default() };
        let parameters = serde_json::json!({});
        let recognized = recognize_visual_with_ledger(
            &gateway,
            vision_input("openai", "gpt-vision", &parameters),
            "image/png",
            &valid_png_bytes(),
            None,
            Some(&ledger),
        )
        .await
        .expect("a paid response is never discarded over accounting");

        assert_eq!(recognized.outcome, VisualTextOutcome::Text { text: "Counted OCR text".into() });
        assert_eq!(recognized.provider_call_id, Some(reserved_call_id()));
        assert_eq!(recognized.calls.len(), 1);
        assert_eq!(recognized.calls[0].usage_json, serde_json::json!({"total_tokens": 7}));
        // The exact observed usage reached settlement before the failure: the
        // reservation owner retries completion with that same payload, so the
        // paid attempt is never downgraded to a no-usage failure here.
        assert_eq!(
            *ledger.settled.lock().expect("ledger mutex"),
            vec![serde_json::json!({"total_tokens": 7})]
        );
        assert_eq!(ledger.failed.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn vision_ledger_fails_the_reserved_row_when_the_request_errors() {
        let gateway = ScriptedVisionGateway { error: true, ..Default::default() };
        let ledger = RecordingLedger::default();
        let parameters = serde_json::json!({});
        let error = recognize_visual_with_ledger(
            &gateway,
            vision_input("openai", "gpt-vision", &parameters),
            "image/png",
            &valid_png_bytes(),
            None,
            Some(&ledger),
        )
        .await
        .err()
        .expect("a provider error is reported");

        assert_eq!(error.provider_call_id, Some(reserved_call_id()));
        assert_eq!(ledger.failed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(ledger.settled.lock().expect("ledger mutex").is_empty());
    }

    #[tokio::test]
    async fn vision_ledger_refusal_never_issues_the_paid_request() {
        let gateway = ScriptedVisionGateway::default();
        let ledger = RecordingLedger { refuse: true, ..Default::default() };
        let parameters = serde_json::json!({});
        let error = recognize_visual_with_ledger(
            &gateway,
            vision_input("openai", "gpt-vision", &parameters),
            "image/png",
            &valid_png_bytes(),
            None,
            Some(&ledger),
        )
        .await
        .err()
        .expect("a refused reservation stops the call");

        assert!(error.terminal);
        assert_eq!(error.provider_call_id, None);
        assert_eq!(gateway.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(ledger.attempts.lock().expect("ledger mutex").is_empty());
    }

    #[tokio::test]
    async fn interpreted_relations_stay_out_of_the_transcribed_text() {
        let gateway = ScriptedVisionGateway { relations: true, ..Default::default() };
        let parameters = serde_json::json!({});
        let output = extract_image_with_provider_with_ledger(
            &gateway,
            vision_input("openai", "gpt-vision", &parameters),
            "image/png",
            &valid_png_bytes(),
            None,
            None,
        )
        .await
        .expect("image extraction");

        assert_eq!(output.content_text, "Orders and customers are drawn as separate boxes");
        assert!(
            !output.content_text.contains("references"),
            "an interpreted relation is not transcribed text"
        );
        let inferred =
            output.source_map["inferred_visual_structure"].as_array().expect("inferred list");
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0]["inferred"], serde_json::json!(true));
        assert_eq!(inferred[0]["promptSha256"], serde_json::json!(prompt_sha256(IMAGE_OCR_PROMPT)));
        assert_eq!(
            inferred[0]["relations"].as_array().map(Vec::len),
            Some(1),
            "structurally empty relations are dropped"
        );
        assert_eq!(inferred[0]["relations"][0]["subject"], serde_json::json!("orders"));
        assert_eq!(
            inferred[0]["relations"][0]["direction"],
            serde_json::json!("orders -> customers")
        );
        assert!(
            output.source_map["visual_recognition"][0]["outcome"].get("relations").is_none(),
            "the stored outcome stays a verbatim transcription"
        );
        assert_eq!(
            output.source_map["visual_recognition"][0]["inferred"],
            serde_json::json!(false)
        );
    }
}
