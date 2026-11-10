use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use rust_decimal::Decimal;

pub(crate) const OPENAI_COMPATIBLE_RUNTIME_KIND: &str = "openai_compatible";

/// Conservative UTF-8 byte bounds for a resolved embedding model. For
/// `cl100k_base`, each ordinary token represents at least one input byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingInputBounds {
    pub max_input_bytes: usize,
    pub max_request_bytes: usize,
    pub max_inputs: usize,
}

impl EmbeddingInputBounds {
    /// The model-specific contract; unknown providers and models are not
    /// assigned an invented limit.
    #[must_use]
    pub fn for_resolved_model(provider_kind: &str, model_name: &str) -> Option<Self> {
        match (provider_kind, model_name) {
            ("openai", "text-embedding-3-large") => {
                Some(Self { max_input_bytes: 8192, max_request_bytes: 300_000, max_inputs: 2048 })
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn accepts<'a>(self, inputs: impl IntoIterator<Item = &'a str>) -> bool {
        let mut total = 0usize;
        let mut count = 0usize;
        for input in inputs {
            if input.is_empty() || input.len() > self.max_input_bytes {
                return false;
            }
            let Some(next) = total.checked_add(input.len()) else { return false };
            total = next;
            count += 1;
            if total > self.max_request_bytes || count > self.max_inputs {
                return false;
            }
        }
        count > 0
    }

    #[must_use]
    pub fn accepts_json(self, input: &serde_json::Value) -> bool {
        match input {
            serde_json::Value::String(text) => self.accepts([text.as_str()]),
            serde_json::Value::Array(values) if values.len() <= self.max_inputs => values
                .iter()
                .map(serde_json::Value::as_str)
                .collect::<Option<Vec<_>>>()
                .is_some_and(|inputs| self.accepts(inputs)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod embedding_input_bounds_tests {
    use super::EmbeddingInputBounds;

    #[test]
    fn known_openai_model_enforces_utf8_and_total_bounds() {
        let bounds = EmbeddingInputBounds::for_resolved_model("openai", "text-embedding-3-large")
            .expect("known model");
        let valid = "ж".repeat(4096);
        let oversized = "ж".repeat(4097);
        assert!(bounds.accepts([valid.as_str()]));
        assert!(!bounds.accepts([oversized.as_str()]));
        assert!(!bounds.accepts([""]));
        assert!(!bounds.accepts(std::iter::empty()));
        assert!(!bounds.accepts(std::iter::repeat_n(valid.as_str(), 37)));
        assert!(!bounds.accepts(std::iter::repeat_n("x", 2049)));
        assert!(!bounds.accepts_json(&serde_json::json!(["valid", 123])));
        assert!(
            EmbeddingInputBounds::for_resolved_model("other", "text-embedding-3-large").is_none()
        );
        assert!(EmbeddingInputBounds::for_resolved_model("openai", "other").is_none());
    }
}
pub(crate) const ANTHROPIC_MESSAGES_RUNTIME_KIND: &str = "anthropic_messages";
pub(crate) const GEMINI_GENERATE_CONTENT_RUNTIME_KIND: &str = "gemini_generate_content";
/// Runtime kind of the TypeSafe System One typed-decision endpoint. Declared
/// here so both the catalog invariant (`ProviderCapabilities.decision` is
/// supported exactly for this runtime kind) and the seed agree on one spelling.
/// The chat gateway does not recognise this kind; semantic rerank (IRG-187) and
/// graph entity typing (IRG-185) dispatch it through the SystemOne client, and
/// no other purpose may use a decision provider.
pub(crate) const TYPESAFE_SYSTEM_ONE_RUNTIME_KIND: &str = "typesafe_system_one";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthScheme {
    Bearer,
    RawAuthorization,
    AnthropicApiKey,
    GoogleApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTokenLimitParameter {
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStructuredOutputMode {
    JsonSchema,
    JsonObject,
    PromptOnlyJsonObject,
    Unsupported,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSamplingPolicy {
    #[default]
    Forward,
    Omit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderToolChoicePolicy {
    #[default]
    RequiredCapable,
    AutoOnly,
}

/// How a request carries retrieved context that no model turn produced.
///
/// `Transcript` is the default and the honest state for an unknown endpoint:
/// the fixed-context answer stages send the same assistant tool-call record
/// and tool result a tool-using agent would see. `Inline` belongs to endpoints
/// whose Chat Completions API rejects tool calls inserted into the middle of a
/// conversation (DeepSeek documents exactly that restriction), and the gateway
/// then carries the same evidence as a named block inside the last user
/// message.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSyntheticToolHistoryPolicy {
    #[default]
    Transcript,
    Inline,
}

/// How the provider endpoint exposes hidden reasoning to a caller whose
/// completion budget is sized for the answer alone.
///
/// `Forward` is the default and the honest state for an unknown endpoint: the
/// request keeps the binding's own parameters and the caller must budget for
/// hidden reasoning. Only a provider whose endpoint accepts the
/// `reasoning_effort` control declares `DisableWithNone`, and the LLM gateway
/// then owns switching it off instead of any caller matching on a provider
/// name.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReasoningPolicy {
    #[default]
    Forward,
    DisableWithNone,
}

impl ProviderReasoningPolicy {
    /// The formal `reasoning_effort` value that disables hidden reasoning, or
    /// `None` when the endpoint exposes no such control.
    #[must_use]
    pub const fn effort_override(self) -> Option<&'static str> {
        match self {
            Self::Forward => None,
            Self::DisableWithNone => Some("none"),
        }
    }
}

/// One reasoning mode an operator may select for a binding.
///
/// The vocabulary is closed and evidence-bound: a variant exists only once a
/// source confirms it. `None` disables hidden reasoning. It is deliberately
/// independent of [`ProviderReasoningPolicy`], which stays the two-state
/// provider-level automatic forwarding policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningMode {
    /// Disable hidden reasoning.
    None,
}

impl ReasoningMode {
    /// The stable name of this mode, as it appears in the typed selection.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

/// The documented vendor wire control a confirmed model pair uses to apply a
/// reasoning mode.
///
/// Each control owns the provider-body parameters it sets, so emission clears
/// every reasoning switch and applies exactly one: two controls can never be
/// sent together. A variant exists only once a first-party source documents
/// its exact wire form; an undocumented control has no variant and its pair
/// stays `Unconfirmed` rather than being guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningWireControl {
    /// OpenAI-compatible `reasoning_effort: "none"`, documented for OpenAI's
    /// reasoning models and for DeepSeek's Chat Completions API.
    ReasoningEffortNone,
    /// DeepSeek `thinking: {"type": "disabled"}`, documented on the DeepSeek
    /// Chat Completions API reference.
    ThinkingDisabled,
}

impl ReasoningWireControl {
    /// Every provider-body parameter any reasoning control owns. Emission
    /// clears all of them before applying the one declared control, so an
    /// automatic `reasoning_effort` can never ride beside a `thinking` control.
    pub(crate) const OWNED_PARAMETERS: [&'static str; 2] = ["reasoning_effort", "thinking"];

    /// Apply this control to a provider-body parameter map.
    pub(crate) fn apply(self, parameters: &mut serde_json::Map<String, serde_json::Value>) {
        for parameter in Self::OWNED_PARAMETERS {
            parameters.remove(parameter);
        }
        match self {
            Self::ReasoningEffortNone => {
                parameters.insert(
                    "reasoning_effort".to_string(),
                    serde_json::Value::String("none".to_string()),
                );
            }
            Self::ThinkingDisabled => {
                parameters.insert("thinking".to_string(), serde_json::json!({"type": "disabled"}));
            }
        }
    }

    /// Whether a binding's own raw parameters disagree with this control.
    /// Only the declared switch with its exact value may be supplied. The other
    /// owned switch is a conflict even if it also disables reasoning. This runs
    /// at the write boundary, before automatic policy injection; emission still
    /// clears automatic controls rather than treating them as caller input.
    /// Returns a message naming the conflict, or `None` when they agree.
    #[must_use]
    pub(crate) fn raw_conflict(self, extra_parameters_json: &serde_json::Value) -> Option<String> {
        let other_parameter = match self {
            Self::ReasoningEffortNone => "thinking",
            Self::ThinkingDisabled => "reasoning_effort",
        };
        if extra_parameters_json.get(other_parameter).is_some() {
            return Some(format!(
                "the raw `{other_parameter}` parameter is not the confirmed wire control"
            ));
        }
        match self {
            Self::ReasoningEffortNone => match extra_parameters_json.get("reasoning_effort") {
                Some(value) if value.as_str() != Some("none") => Some(
                    "the raw `reasoning_effort` parameter disagrees with the confirmed `reasoning_effort: none` control"
                        .to_string(),
                ),
                _ => None,
            },
            Self::ThinkingDisabled => match extra_parameters_json.get("thinking") {
                Some(value) if value != &serde_json::json!({"type": "disabled"}) => Some(
                    "the raw `thinking` parameter disagrees with the confirmed `thinking: {\"type\": \"disabled\"}` control"
                        .to_string(),
                ),
                _ => None,
            },
        }
    }
}

/// The reasoning capability of one exact provider/model pair, owned by that
/// model's catalog metadata row.
///
/// This is pair knowledge, not a provider policy: it answers "may an operator
/// select mode X for this exact model", which a provider-wide value cannot. It
/// is stored at `metadataJson.reasoningCapability` on the model catalog entry
/// and never on the provider profile, so a provider-level grant can never
/// authorize a model that was not itself confirmed. `Unconfirmed` is the
/// default and the honest state of any pair nobody verified — it grants no
/// mode. `Confirmed` records the mode, the documented wire control that applies
/// it, and the first-party evidence that established the pair; all are
/// required, and a confirmation with no recorded provenance is rejected.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelReasoningCapability {
    /// No mode is confirmed for this exact model; nothing may be selected.
    #[default]
    Unconfirmed,
    /// The pair confirms `mode` through `wire`, established by `evidence`.
    Confirmed {
        mode: ReasoningMode,
        wire: ReasoningWireControl,
        /// First-party reference that confirmed this exact provider/model
        /// pair and its wire form. Operator catalog data, never inferred from
        /// a provider or model name.
        evidence: String,
    },
}

impl ModelReasoningCapability {
    /// Whether the pair confirms `mode`, i.e. an operator may select it.
    #[must_use]
    pub(crate) fn allows(&self, mode: ReasoningMode) -> bool {
        matches!(self, Self::Confirmed { mode: confirmed, .. } if *confirmed == mode)
    }

    /// The documented wire control for the confirmed mode, if any.
    #[must_use]
    pub(crate) fn wire(&self) -> Option<ReasoningWireControl> {
        match self {
            Self::Unconfirmed => None,
            Self::Confirmed { wire, .. } => Some(*wire),
        }
    }

    /// Validate that a confirmation is attributed. An `Unconfirmed` pair is
    /// always valid; a `Confirmed` pair with blank evidence is a data error.
    ///
    /// # Errors
    /// Returns a message naming the problem.
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Unconfirmed => Ok(()),
            Self::Confirmed { evidence, .. } if evidence.trim().is_empty() => Err(
                "reasoningCapability.confirmed.evidence must name the source that confirmed the exact provider/model pair"
                    .to_string(),
            ),
            Self::Confirmed { .. } => Ok(()),
        }
    }
}

/// Version stamped into every typed selected-mode wire form.
///
/// Explicit so a future vocabulary change is told apart from this form instead
/// of being read as a gap, and an unknown version fails closed rather than
/// being guessed.
pub(crate) const REASONING_MODE_FORM_VERSION: u32 = 1;

/// The canonical key the typed selected mode occupies inside a binding's
/// `extra_parameters_json`. The surrounding JSONB stays free-form; this key is
/// the one field the runtime reads as a typed selection.
pub(crate) const REASONING_MODE_FIELD: &str = "reasoningMode";

/// The canonical key a model's pair reasoning capability occupies inside the
/// model catalog entry's `metadata_json`.
pub(crate) const MODEL_REASONING_CAPABILITY_FIELD: &str = "reasoningCapability";

/// The reserved internal key the server projects the exact pair reasoning
/// capability into on a resolved binding. It is overwritten unconditionally
/// from the bound model's catalog row at resolution, so a client binding can
/// never grant, override or inject it through its own JSON.
pub(crate) const RESOLVED_REASONING_CAPABILITY_FIELD: &str = "_providerModelReasoning";

/// The typed selected reasoning mode as stored in one binding's
/// `extra_parameters_json`: an explicit version beside the mode. Parsed only
/// through [`Self::from_binding_json`] so an unknown or versionless form is
/// rejected instead of silently treated as unset. The operator authors this
/// JSON; the runtime only reads it, validates it against the exact model's
/// capability, and emits the confirmed wire control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SelectedReasoningMode {
    version: u32,
    mode: ReasoningMode,
}

impl SelectedReasoningMode {
    /// The selected mode.
    #[must_use]
    pub(crate) const fn mode(self) -> ReasoningMode {
        self.mode
    }

    /// Read the typed selection out of a binding's `extra_parameters_json`.
    ///
    /// `Ok(None)` is the honest state of a binding that carries no typed field:
    /// no mode was ever selected, so the existing automatic policy and the
    /// binding's own raw parameters apply untouched. A present-but-malformed
    /// field — wrong version, unknown mode, wrong type, unknown sibling key —
    /// is an error, never read as unset.
    ///
    /// # Errors
    /// Returns a message naming the reason when the field is present and
    /// cannot be read as a canonical form.
    pub(crate) fn from_binding_json(
        extra_parameters_json: &serde_json::Value,
    ) -> Result<Option<Self>, String> {
        let Some(value) = extra_parameters_json.get(REASONING_MODE_FIELD) else {
            return Ok(None);
        };
        let selection = serde_json::from_value::<Self>(value.clone()).map_err(|error| {
            format!("extraParametersJson.{REASONING_MODE_FIELD} is invalid: {error}")
        })?;
        if selection.version != REASONING_MODE_FORM_VERSION {
            return Err(format!(
                "extraParametersJson.{REASONING_MODE_FIELD} has unsupported version {}; expected {}",
                selection.version, REASONING_MODE_FORM_VERSION
            ));
        }
        Ok(Some(selection))
    }
}

/// Read and validate the pair reasoning capability from a model catalog
/// entry's `metadataJson`. An absent key is the honest `Unconfirmed` state.
///
/// # Errors
/// Returns a message when the field is present but malformed.
pub(crate) fn model_reasoning_capability(
    metadata_json: &serde_json::Value,
) -> Result<ModelReasoningCapability, String> {
    let capability = match metadata_json.get(MODEL_REASONING_CAPABILITY_FIELD) {
        Some(value) => {
            serde_json::from_value::<ModelReasoningCapability>(value.clone()).map_err(|error| {
                format!("metadataJson.{MODEL_REASONING_CAPABILITY_FIELD} is invalid: {error}")
            })?
        }
        None => ModelReasoningCapability::Unconfirmed,
    };
    capability.validate()?;
    Ok(capability)
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRequestPolicy {
    #[serde(default)]
    pub sampling: ProviderSamplingPolicy,
    #[serde(default)]
    pub tool_choice: ProviderToolChoicePolicy,
    #[serde(default)]
    pub default_tool_max_output_tokens: Option<i32>,
    #[serde(default)]
    pub reasoning: ProviderReasoningPolicy,
    #[serde(default)]
    pub synthetic_tool_history: ProviderSyntheticToolHistoryPolicy,
}

impl ProviderRequestPolicy {
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        match self.default_tool_max_output_tokens {
            Some(value) => value > 0,
            None => true,
        }
    }

    /// Whether hidden reasoning can be switched off for an answer-only budget.
    #[must_use]
    pub const fn disables_hidden_reasoning(&self) -> bool {
        self.reasoning.effort_override().is_some()
    }

    /// The formal `reasoning_effort` value to apply, if any.
    #[must_use]
    pub const fn reasoning_effort_override(&self) -> Option<&'static str> {
        self.reasoning.effort_override()
    }

    /// Whether the endpoint needs synthetic tool history carried inside the
    /// last user message instead of an inserted tool-call transcript.
    #[must_use]
    pub const fn inlines_synthetic_tool_history(&self) -> bool {
        matches!(self.synthetic_tool_history, ProviderSyntheticToolHistoryPolicy::Inline)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsagePolicy {
    /// Select token accounting only from formal usage-payload fields.
    #[default]
    AutoDetect,
    /// Cached input is reported as a subset of total request input.
    CachedSubsetOfInput,
    /// Cache-read and cache-write counters are disjoint from ordinary input.
    DisjointCacheCounters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRuntimeProfile {
    pub kind: String,
    pub auth_scheme: ProviderAuthScheme,
    pub token_limit_parameter: ProviderTokenLimitParameter,
    pub structured_output: ProviderStructuredOutputMode,
    pub chat_path: String,
    pub embeddings_path: Option<String>,
    pub models_path: Option<String>,
    /// Ordered relevance-level descriptions (lowest to highest) that one
    /// SystemOne `Score` question carries when a decision runtime scores a
    /// semantic-rerank shortlist. The protocol accepts `2..=10` levels, and a
    /// decision-runtime provider must declare them: the rerank fails closed
    /// instead of scoring against an invented rubric. This is catalog policy,
    /// not query code — the rerank reads it from the resolved profile.
    #[serde(default)]
    pub score_criteria: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBaseUrlMode {
    Fixed,
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialPolicy {
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub base_url_mode: ProviderBaseUrlMode,
    pub validation_mode: ProviderCredentialValidationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCredentialValidationMode {
    ChatRoundTrip,
    ModelList,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBaseUrlPolicy {
    pub allow_override: bool,
    pub require_https: bool,
    pub allow_private_network: bool,
    #[serde(default)]
    pub trim_suffixes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderModelDiscoveryMode {
    Shared,
    Credential,
    Unsupported,
}

/// Capability family a discovery endpoint returns. Re-exported from the
/// public contract type rather than duplicated: the wire shape (chat /
/// embedding / decision) is one vocabulary shared by the operator-facing API
/// and the internally stored provider profile.
pub(crate) use ironrag_contracts::provider::ProviderModelCapabilityKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelDiscoveryPath {
    pub capability_kind: ProviderModelCapabilityKind,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelDiscovery {
    pub mode: ProviderModelDiscoveryMode,
    pub paths: Vec<ProviderModelDiscoveryPath>,
    /// Currency the provider's listing quotes its rates in. OpenAI-compatible
    /// listings carry a bare number and no unit, so this has to be declared:
    /// a Russian router quotes roubles in the same field a US one quotes
    /// dollars, and reading one as the other overstates spend a hundredfold.
    /// Absent means the unit is unknown, and a discovered model then carries
    /// no price rather than a wrong one.
    #[serde(default)]
    pub listed_pricing_currency: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

impl ProviderCapabilityState {
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// The honest default for a capability a persisted row predates: absent
    /// means the catalog never declared it, i.e. `Unsupported`.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self::Unsupported
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub chat: ProviderCapabilityState,
    pub embeddings: ProviderCapabilityState,
    pub vision: ProviderCapabilityState,
    pub streaming: ProviderCapabilityState,
    pub tools: ProviderCapabilityState,
    pub model_discovery: ProviderCapabilityState,
    /// Typed decision-model availability (TypeSafe System One). Supported
    /// exactly when `runtime.kind == TYPESAFE_SYSTEM_ONE_RUNTIME_KIND`;
    /// `parse_provider_profile` enforces that invariant. Defaults to
    /// `Unsupported` so a row persisted before this capability existed
    /// deserializes rather than failing to parse.
    #[serde(default = "ProviderCapabilityState::unsupported")]
    pub decision: ProviderCapabilityState,
    /// Whether the endpoint accepts `service_tier: "flex"`, the half-price
    /// tier for latency-tolerant work. Unsupported by default: an endpoint
    /// that does not declare it may reject the parameter.
    #[serde(default = "ProviderCapabilityState::unsupported")]
    pub flex_tier: ProviderCapabilityState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub runtime: ProviderRuntimeProfile,
    pub credentials: ProviderCredentialPolicy,
    pub base_url: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    #[serde(default)]
    pub request_policy: ProviderRequestPolicy,
    #[serde(default)]
    pub usage_policy: ProviderUsagePolicy,
    /// Price multiplier the catalog declares per actual `service_tier` a
    /// provider response confirms. Vendor rules are catalog data, never a
    /// branch on a provider or model name: OpenAI prices `flex` at half the
    /// standard rate and `priority` at double. A tier absent from the map —
    /// including the standard `default` — prices at 1.
    #[serde(default)]
    pub service_tier_multipliers: BTreeMap<String, Decimal>,
    /// Durable admission state belongs to the provider catalog, not its runtime projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub financial_admission: Option<FinancialAdmission>,
    /// Binding purposes no seeded model of this provider covers, each with the
    /// reason a person can act on. Setup shows these next to the provider so an
    /// operator resolves the gap instead of discovering it when their first
    /// document fails to ingest.
    #[serde(default)]
    pub purpose_gaps: Vec<ProviderPurposeGap>,
    #[serde(default)]
    pub ui_hints: serde_json::Value,
}

/// A non-replenishing grant. Monetary amounts use integer millionths of USD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinancialAdmission {
    pub version: u32,
    pub provider_id: uuid::Uuid,
    pub scope_id: uuid::Uuid,
    pub policy_fingerprint: String,
    pub cutoff: chrono::DateTime<chrono::Utc>,
    pub state: FinancialAdmissionState,
    pub grant_microusd: u64,
    pub global_limit: u64,
    pub global_consumed: u64,
    pub models: BTreeMap<String, FinancialModelAllowance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<FinancialAdmissionArchive>,
}

/// A closed grant retained without nested history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinancialAdmissionArchive {
    pub version: u32,
    pub provider_id: uuid::Uuid,
    pub scope_id: uuid::Uuid,
    pub policy_fingerprint: String,
    pub cutoff: chrono::DateTime<chrono::Utc>,
    pub state: FinancialAdmissionState,
    pub grant_microusd: u64,
    pub global_limit: u64,
    pub global_consumed: u64,
    pub models: BTreeMap<String, FinancialModelAllowance>,
}

impl From<FinancialAdmission> for FinancialAdmissionArchive {
    fn from(value: FinancialAdmission) -> Self {
        Self {
            version: value.version,
            provider_id: value.provider_id,
            scope_id: value.scope_id,
            policy_fingerprint: value.policy_fingerprint,
            cutoff: value.cutoff,
            state: value.state,
            grant_microusd: value.grant_microusd,
            global_limit: value.global_limit,
            global_consumed: value.global_consumed,
            models: value.models,
        }
    }
}

impl From<FinancialAdmissionArchive> for FinancialAdmission {
    fn from(value: FinancialAdmissionArchive) -> Self {
        Self {
            version: value.version,
            provider_id: value.provider_id,
            scope_id: value.scope_id,
            policy_fingerprint: value.policy_fingerprint,
            cutoff: value.cutoff,
            state: value.state,
            grant_microusd: value.grant_microusd,
            global_limit: value.global_limit,
            global_consumed: value.global_consumed,
            models: value.models,
            history: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FinancialAdmissionState {
    Prepared,
    Active,
    Closed,
}

/// The send boundary must enforce the entire selected envelope without rewriting the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinancialModelAllowance {
    pub endpoint: String,
    pub envelope: FinancialRequestEnvelope,
    pub attempt_microusd: u64,
    pub limit: u64,
    pub consumed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
/// One admitted request class.
///
/// The financial unit is the full published model maximum: one reservation per
/// physical attempt, taken non-refundably before dispatch and never reduced by
/// the measured body size. It rests on the documented model input maximum
/// (`maxInputTokens`) and the documented per-token tariff, reserved at the
/// highest documented input rate.
///
/// A provider that rejects an oversized request and bills it anyway is a named
/// contract-deviation assumption: there is no numeric bound for it and no claim
/// that unallocated money covers it.
///
/// The byte fields below are resource and validity guards. They bound memory and
/// obviously unacceptable payloads, they are not a token-count proof and not a
/// billing bound, and they never lower the reservation.
///
/// Embedding class: the byte bound additionally rests on first-party evidence
/// that `text-embedding-3-large` maps to `cl100k_base` at the pinned tiktoken
/// revision and that the encoder emits tokens only as non-empty UTF-8 byte
/// spans. Inputs must therefore be strings; token-ID arrays are refused,
/// because that reasoning does not cover them.
///
/// Generation class: that encoding premise is NOT established and is not
/// claimed. `maxInputTokens` there is the documented model maximum the
/// reservation is taken at, not a proof of the provider's tokenization.
pub enum FinancialRequestEnvelope {
    Chat {
        allowed_service_tiers: Vec<String>,
        /// Resource and validity guard only; never a billing bound.
        max_input_bytes: u64,
        max_output_tokens: u64,
        /// Documented model input maximum the reservation is taken at.
        max_input_tokens: u64,
    },
    Embeddings {
        max_inputs: u64,
        /// Resource and validity guard only.
        max_input_bytes_each: u64,
        /// Resource and validity guard only.
        max_input_bytes_total: u64,
        /// Documented aggregate model input maximum. No default.
        max_input_tokens: u64,
    },
}

impl FinancialAdmission {
    /// Apply a lifecycle change; the server constructs successor history.
    pub(crate) fn transition(
        current: Option<Self>,
        mut desired: Self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Self, &'static str> {
        desired.validate()?;
        if desired.immutable_fingerprint().ok().as_deref() != Some(&desired.policy_fingerprint) {
            return Err("invalid financial admission fingerprint");
        }
        let fresh = || {
            desired.state == FinancialAdmissionState::Prepared
                && desired.global_consumed == 0
                && desired.models.values().all(|model| model.consumed == 0)
                && desired.cutoff > now
        };
        match current {
            None if fresh() && desired.history.is_empty() => Ok(desired),
            None => Err("invalid initial financial admission"),
            Some(current) => {
                current.validate()?;
                if current.provider_id != desired.provider_id
                    || current.immutable_fingerprint().ok().as_deref()
                        != Some(&current.policy_fingerprint)
                {
                    return Err("invalid current financial admission");
                }
                if current.state == FinancialAdmissionState::Closed
                    && desired.state == FinancialAdmissionState::Prepared
                {
                    if !fresh()
                        || current.scope_id == desired.scope_id
                        || (!desired.history.is_empty() && desired.history != current.history)
                    {
                        return Err("invalid successor financial admission");
                    }
                    desired.history = current.history.clone();
                    desired.history.push(current.into());
                    Ok(desired)
                } else {
                    if desired.history.is_empty() {
                        desired.history = current.history.clone();
                    } else if desired.history != current.history {
                        return Err("invalid financial admission history");
                    }
                    let mut expected = current.clone();
                    expected.state = desired.state;
                    if expected != desired
                        || !matches!(
                            (current.state, desired.state),
                            (FinancialAdmissionState::Prepared, FinancialAdmissionState::Active)
                                | (
                                    FinancialAdmissionState::Active,
                                    FinancialAdmissionState::Closed
                                )
                        )
                        || (desired.state == FinancialAdmissionState::Active
                            && desired.cutoff <= now)
                    {
                        return Err("invalid financial admission transition");
                    }
                    Ok(desired)
                }
            }
        }
    }

    /// SHA-256 of the versioned immutable policy, excluding lifecycle and counters.
    pub fn immutable_fingerprint(&self) -> Result<String, serde_json::Error> {
        use sha2::{Digest, Sha256};
        #[derive(Serialize)]
        struct ImmutableModel<'a> {
            endpoint: &'a str,
            envelope: &'a FinancialRequestEnvelope,
            attempt_microusd: u64,
            limit: u64,
        }
        let models: BTreeMap<&str, ImmutableModel<'_>> = self
            .models
            .iter()
            .map(|(name, model)| {
                (
                    name.as_str(),
                    ImmutableModel {
                        endpoint: &model.endpoint,
                        envelope: &model.envelope,
                        attempt_microusd: model.attempt_microusd,
                        limit: model.limit,
                    },
                )
            })
            .collect();
        let bytes = serde_json::to_vec(&(
            "financial-admission/1",
            self.version,
            self.provider_id,
            self.scope_id,
            self.cutoff,
            self.grant_microusd,
            self.global_limit,
            models,
        ))?;
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
    }

    /// Validate internal arithmetic without interpreting an upstream response.
    pub fn validate(&self) -> Result<(), &'static str> {
        for entry in &self.history {
            let archived = FinancialAdmission::from(entry.clone());
            if archived.state != FinancialAdmissionState::Closed
                || archived.provider_id != self.provider_id
                || archived.validate().is_err()
                || archived.immutable_fingerprint().ok().as_deref()
                    != Some(&archived.policy_fingerprint)
            {
                return Err("invalid financial admission history");
            }
        }
        if self.version != 1
            || self.provider_id.is_nil()
            || self.scope_id.is_nil()
            || self.policy_fingerprint.is_empty()
            || self.models.is_empty()
            || self.global_limit == 0
            || self.global_consumed > self.global_limit
        {
            return Err("invalid financial admission identity or limit");
        }
        let mut liability = 0_u64;
        let mut consumed = 0_u64;
        for (model, allowance) in &self.models {
            if model.is_empty()
                || allowance.endpoint.is_empty()
                || allowance.attempt_microusd == 0
                || allowance.limit == 0
                || allowance.consumed > allowance.limit
            {
                return Err("invalid model allowance");
            }
            match &allowance.envelope {
                FinancialRequestEnvelope::Chat {
                    allowed_service_tiers,
                    max_input_bytes,
                    max_output_tokens,
                    max_input_tokens,
                } => {
                    if allowed_service_tiers.is_empty()
                        || allowed_service_tiers
                            .iter()
                            .any(|tier| tier != "default" && tier != "flex")
                        || *max_input_bytes == 0
                        || *max_output_tokens == 0
                    {
                        return Err("invalid chat envelope");
                    }
                    if *max_input_tokens == 0 {
                        return Err("invalid chat input ceiling");
                    }
                }
                FinancialRequestEnvelope::Embeddings {
                    max_inputs,
                    max_input_bytes_each,
                    max_input_bytes_total,
                    max_input_tokens,
                } => {
                    if *max_inputs == 0 || *max_input_bytes_each == 0 || *max_input_bytes_total == 0
                    {
                        return Err("invalid embedding envelope");
                    }
                    if *max_input_tokens == 0 {
                        return Err("invalid embedding input ceiling");
                    }
                }
            }
            liability = liability
                .checked_add(
                    allowance
                        .attempt_microusd
                        .checked_mul(allowance.limit)
                        .ok_or("financial liability overflow")?,
                )
                .ok_or("financial liability overflow")?;
            consumed =
                consumed.checked_add(allowance.consumed).ok_or("financial count overflow")?;
        }
        if liability > self.grant_microusd || consumed != self.global_consumed {
            return Err("financial liability or counters inconsistent");
        }
        Ok(())
    }
}

#[cfg(test)]
mod financial_admission_tests {
    use super::*;

    fn policy() -> FinancialAdmission {
        FinancialAdmission {
            version: 1,
            provider_id: uuid::Uuid::new_v4(),
            scope_id: uuid::Uuid::new_v4(),
            policy_fingerprint: "fixture".to_owned(),
            cutoff: chrono::Utc::now(),
            state: FinancialAdmissionState::Prepared,
            grant_microusd: 100,
            global_limit: 2,
            global_consumed: 0,
            history: Vec::new(),
            models: BTreeMap::from([(
                "fixture-model".to_owned(),
                FinancialModelAllowance {
                    endpoint: "https://example.test/chat/completions".to_owned(),
                    envelope: FinancialRequestEnvelope::Chat {
                        allowed_service_tiers: vec!["default".to_owned(), "flex".to_owned()],
                        max_input_bytes: 100,
                        max_output_tokens: 10,
                        max_input_tokens: 90,
                    },
                    attempt_microusd: 50,
                    limit: 2,
                    consumed: 0,
                },
            )]),
        }
    }

    #[test]
    fn financial_successor_preserves_flat_history_and_fingerprint() {
        let now = chrono::Utc::now();
        let mut old = policy();
        old.cutoff = now + chrono::Duration::hours(1);
        old.state = FinancialAdmissionState::Closed;
        old.global_consumed = 1;
        old.models.get_mut("fixture-model").unwrap().consumed = 1;
        old.policy_fingerprint = old.immutable_fingerprint().unwrap();
        let mut desired = policy();
        desired.provider_id = old.provider_id;
        desired.cutoff = now + chrono::Duration::hours(2);
        desired.policy_fingerprint = desired.immutable_fingerprint().unwrap();
        let fingerprint = desired.policy_fingerprint.clone();
        let stored =
            FinancialAdmission::transition(Some(old.clone()), desired.clone(), now).unwrap();
        assert_eq!(stored.history, vec![FinancialAdmissionArchive::from(old.clone())]);
        assert_eq!(stored.immutable_fingerprint().unwrap(), fingerprint);
        let roundtrip: FinancialAdmission =
            serde_json::from_value(serde_json::to_value(&stored).unwrap()).unwrap();
        assert_eq!(stored, roundtrip);
        assert!(stored.validate().is_ok());
        let mut second_closed = stored.clone();
        second_closed.state = FinancialAdmissionState::Closed;
        let mut third = policy();
        third.provider_id = old.provider_id;
        third.cutoff = now + chrono::Duration::hours(3);
        third.policy_fingerprint = third.immutable_fingerprint().unwrap();
        let second_successor =
            FinancialAdmission::transition(Some(second_closed.clone()), third, now).unwrap();
        assert_eq!(second_successor.history.len(), 2);
        assert_eq!(second_successor.history[0], FinancialAdmissionArchive::from(old.clone()));
        assert_eq!(second_successor.history[1], FinancialAdmissionArchive::from(second_closed));
        let mut nested = serde_json::to_value(&second_successor).unwrap();
        nested["history"][0]["history"] = serde_json::json!([]);
        assert!(serde_json::from_value::<FinancialAdmission>(nested).is_err());

        let mut active = stored.clone();
        active.state = FinancialAdmissionState::Active;
        active.history.clear();
        let activated = FinancialAdmission::transition(Some(stored.clone()), active, now).unwrap();
        assert_eq!(activated.state, FinancialAdmissionState::Active);
        assert_eq!(activated.history, stored.history);

        let mut corrupt = stored;
        corrupt.history[0].global_consumed = 0;
        assert!(corrupt.validate().is_err());
    }

    #[test]
    fn financial_successor_rejects_invalid_candidates() {
        let now = chrono::Utc::now();
        let mut old = policy();
        old.cutoff = now + chrono::Duration::hours(1);
        old.state = FinancialAdmissionState::Closed;
        old.global_consumed = 1;
        old.models.get_mut("fixture-model").unwrap().consumed = 1;
        old.policy_fingerprint = old.immutable_fingerprint().unwrap();
        let mut desired = policy();
        desired.provider_id = old.provider_id;
        desired.cutoff = now + chrono::Duration::hours(2);
        desired.policy_fingerprint = desired.immutable_fingerprint().unwrap();

        let mut same = desired.clone();
        same.scope_id = old.scope_id;
        same.policy_fingerprint = same.immutable_fingerprint().unwrap();
        assert!(FinancialAdmission::transition(Some(old.clone()), same, now).is_err());
        let mut consumed = desired.clone();
        consumed.global_consumed = 1;
        consumed.models.get_mut("fixture-model").unwrap().consumed = 1;
        assert!(FinancialAdmission::transition(Some(old.clone()), consumed, now).is_err());
        let mut expired = desired.clone();
        expired.cutoff = now;
        expired.policy_fingerprint = expired.immutable_fingerprint().unwrap();
        assert!(FinancialAdmission::transition(Some(old.clone()), expired, now).is_err());
        let mut forged = desired;
        forged.policy_fingerprint = "wrong".into();
        assert!(FinancialAdmission::transition(Some(old), forged, now).is_err());
    }

    #[test]
    fn financial_existing_lifecycle_remains_strict() {
        let now = chrono::Utc::now();
        let mut prepared = policy();
        prepared.cutoff = now + chrono::Duration::hours(1);
        prepared.policy_fingerprint = prepared.immutable_fingerprint().unwrap();
        let mut active = prepared.clone();
        active.state = FinancialAdmissionState::Active;
        assert!(
            FinancialAdmission::transition(Some(prepared.clone()), active.clone(), now).is_ok()
        );
        let mut closed = active.clone();
        closed.state = FinancialAdmissionState::Closed;
        assert!(FinancialAdmission::transition(Some(active), closed.clone(), now).is_ok());
        assert!(FinancialAdmission::transition(Some(closed), prepared, now).is_err());
    }

    #[test]
    fn financial_policy_fingerprint_is_stable_for_the_same_policy() {
        let value = policy();
        assert_eq!(value.immutable_fingerprint().unwrap(), value.immutable_fingerprint().unwrap());
    }

    #[test]
    fn financial_policy_fingerprint_ignores_mutable_state_and_counters() {
        let original = policy();
        let mut mutated = original.clone();
        mutated.state = FinancialAdmissionState::Active;
        mutated.global_consumed = 1;
        mutated.models.get_mut("fixture-model").unwrap().consumed = 1;
        assert_eq!(
            mutated.immutable_fingerprint().unwrap(),
            original.immutable_fingerprint().unwrap(),
            "lifecycle and consumption are not part of the immutable policy"
        );
    }

    #[test]
    fn financial_policy_fingerprint_changes_on_every_immutable_field() {
        let base = policy().immutable_fingerprint().unwrap();
        let mut variants: Vec<FinancialAdmission> = Vec::new();

        let mut value = policy();
        value.models.get_mut("fixture-model").unwrap().endpoint.push_str("/other");
        variants.push(value);

        let mut value = policy();
        if let FinancialRequestEnvelope::Chat { max_input_tokens, .. } =
            &mut value.models.get_mut("fixture-model").unwrap().envelope
        {
            *max_input_tokens += 1;
        }
        variants.push(value);

        let mut value = policy();
        value.models.get_mut("fixture-model").unwrap().attempt_microusd += 1;
        variants.push(value);

        let mut value = policy();
        value.models.get_mut("fixture-model").unwrap().limit += 1;
        variants.push(value);

        let mut value = policy();
        value.cutoff += chrono::Duration::seconds(1);
        variants.push(value);

        let mut value = policy();
        value.grant_microusd += 1_000;
        variants.push(value);

        let mut value = policy();
        value.global_limit += 1;
        variants.push(value);

        for variant in variants {
            assert_ne!(
                variant.immutable_fingerprint().unwrap(),
                base,
                "an immutable policy change must move the fingerprint"
            );
        }
    }

    #[test]
    fn financial_policy_deserialization_refusal_never_echoes_the_payload() {
        let secret = "sk-live-should-never-appear";
        let malformed = format!(r#"{{"providerId":"{secret}","models":"#);
        let error = serde_json::from_str::<FinancialAdmission>(&malformed)
            .expect_err("malformed policy must be refused");
        let rendered = error.to_string();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("http://"));
        assert!(!rendered.contains("https://"));
    }

    #[test]
    fn financial_policy_rejects_overflow_overgrant_and_counter_disagreement() {
        let mut value = policy();
        assert!(value.validate().is_ok());
        value.grant_microusd = 99;
        assert!(value.validate().is_err());
        value.grant_microusd = u64::MAX;
        value.models.get_mut("fixture-model").unwrap().attempt_microusd = u64::MAX;
        assert!(value.validate().is_err());
        value = policy();
        value.global_consumed = 1;
        assert!(value.validate().is_err());
    }

    #[test]
    fn financial_policy_rejects_zero_input_ceiling() {
        let mut value = policy();
        assert!(value.validate().is_ok());
        let model = value.models.get_mut("fixture-model").unwrap();
        if let FinancialRequestEnvelope::Chat { max_input_tokens, .. } = &mut model.envelope {
            *max_input_tokens = 0;
        }
        assert!(value.validate().is_err());
    }

    #[test]
    fn financial_envelope_deserialization_requires_the_input_ceiling() {
        let missing = serde_json::json!({
            "kind": "chat",
            "allowed_service_tiers": ["default"],
            "max_input_bytes": 100,
            "max_output_tokens": 10
        });
        assert!(serde_json::from_value::<FinancialRequestEnvelope>(missing).is_err());

        let present = serde_json::json!({
            "kind": "chat",
            "allowed_service_tiers": ["default"],
            "max_input_bytes": 100,
            "max_output_tokens": 10,
            "max_input_tokens": 90
        });
        assert!(serde_json::from_value::<FinancialRequestEnvelope>(present).is_ok());
    }

    #[test]
    fn financial_policy_rejects_auto_tier() {
        let mut value = policy();
        let model = value.models.get_mut("fixture-model").unwrap();
        if let FinancialRequestEnvelope::Chat { allowed_service_tiers, .. } = &mut model.envelope {
            allowed_service_tiers.push("auto".to_owned());
        }
        assert!(value.validate().is_err());
    }

    #[test]
    fn financial_fingerprint_ignores_mutable_state_but_not_policy() {
        let mut value = policy();
        let original = value.immutable_fingerprint().unwrap();
        value.state = FinancialAdmissionState::Active;
        value.global_consumed = 1;
        value.models.get_mut("fixture-model").unwrap().consumed = 1;
        assert_eq!(value.immutable_fingerprint().unwrap(), original);
        value.models.get_mut("fixture-model").unwrap().limit = 1;
        assert_ne!(value.immutable_fingerprint().unwrap(), original);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPurposeGap {
    pub purpose: crate::domains::ai::AiBindingPurpose,
    pub kind: ProviderPurposeGapKind,
    /// Stable code a client localises. `reason` is the English fallback for
    /// callers with no catalogue of their own.
    #[serde(default)]
    pub code: String,
    pub reason: String,
}

/// Why a purpose has no seeded model. The distinction is load-bearing: a
/// provider that cannot embed is a dead end the operator must route around,
/// while a router with no seeded chat model serves chat perfectly well as soon
/// as a model is chosen from its live listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPurposeGapKind {
    ProviderCannot,
    OperatorSupplies,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderModelSelection {
    pub provider_kind: String,
    pub model_name: String,
}

#[cfg(test)]
mod reasoning_capability_tests {
    use super::{
        ModelReasoningCapability, REASONING_MODE_FIELD, ReasoningMode, ReasoningWireControl,
        SelectedReasoningMode, model_reasoning_capability,
    };

    const OPENAI_EVIDENCE: &str = "https://developers.openai.com/api/docs/guides/reasoning";
    const DEEPSEEK_EVIDENCE: &str = "https://api-docs.deepseek.com/api/create-chat-completion/";

    fn confirmed_capability(mode: &str, wire: &str, evidence: &str) -> ModelReasoningCapability {
        serde_json::from_value(serde_json::json!({
            "confirmed": {"mode": mode, "wire": wire, "evidence": evidence}
        }))
        .expect("a confirmed capability fixture must parse")
    }

    #[test]
    fn selected_mode_round_trips_and_foreign_parameters_are_preserved() {
        // The operator authors the JSONB; the runtime only reads it. Foreign
        // parameters must survive read/emit untouched.
        let extra = serde_json::json!({
            "temperature": 0.2,
            "vendor_option": {"nested": [1, 2, 3]},
            "large_cost": "x".repeat(4096),
            "service_tier": "flex",
            "reasoningMode": {"version": 1, "mode": "none"},
        });
        let parsed = SelectedReasoningMode::from_binding_json(&extra)
            .expect("a canonical selection must parse")
            .expect("a present selection must be returned");
        assert_eq!(parsed.mode(), ReasoningMode::None);
        assert_eq!(extra["service_tier"], serde_json::json!("flex"));
        assert_eq!(extra["vendor_option"], serde_json::json!({"nested": [1, 2, 3]}));
        assert_eq!(extra[REASONING_MODE_FIELD], serde_json::json!({"version": 1, "mode": "none"}));
    }

    #[test]
    fn absent_field_is_unset_not_none() {
        let legacy = serde_json::json!({"reasoning_effort": "high", "service_tier": "flex"});
        assert_eq!(
            SelectedReasoningMode::from_binding_json(&legacy),
            Ok(None),
            "an absent field is the honest unset state, not an error"
        );
        assert_eq!(SelectedReasoningMode::from_binding_json(&serde_json::json!({})), Ok(None));
    }

    #[test]
    fn unknown_version_or_mode_is_rejected_never_silently_unset() {
        let unknown_version = serde_json::json!({"reasoningMode": {"version": 99, "mode": "none"}});
        let error = SelectedReasoningMode::from_binding_json(&unknown_version)
            .expect_err("an unknown form version must fail closed");
        assert!(error.contains("unsupported version"), "{error}");

        let versionless = serde_json::json!({"reasoningMode": {"mode": "none"}});
        assert!(
            SelectedReasoningMode::from_binding_json(&versionless).is_err(),
            "a versionless form is not the canonical shape"
        );

        let unknown_mode = serde_json::json!({"reasoningMode": {"version": 1, "mode": "turbo"}});
        let error = SelectedReasoningMode::from_binding_json(&unknown_mode)
            .expect_err("an unknown mode must fail closed");
        assert!(error.contains("invalid"), "{error}");

        let wrong_typed_value = serde_json::json!({"reasoningMode": "none"});
        assert!(
            SelectedReasoningMode::from_binding_json(&wrong_typed_value).is_err(),
            "a bare string is not the canonical object form"
        );

        let extra_sibling = serde_json::json!({
            "reasoningMode": {"version": 1, "mode": "none", "sneaky": true}
        });
        assert!(
            SelectedReasoningMode::from_binding_json(&extra_sibling).is_err(),
            "an unknown sibling key inside the typed form must fail closed"
        );
    }

    #[test]
    fn capability_is_model_metadata_with_a_documented_wire_control() {
        let metadata = serde_json::json!({
            "reasoningCapability": {
                "confirmed": {"mode": "none", "wire": "reasoning_effort_none",
                              "evidence": OPENAI_EVIDENCE}
            }
        });
        let capability = model_reasoning_capability(&metadata).expect("valid confirmation");
        assert!(capability.allows(ReasoningMode::None));
        assert_eq!(capability.wire(), Some(ReasoningWireControl::ReasoningEffortNone));

        let unconfirmed = model_reasoning_capability(&serde_json::json!({})).expect("absent");
        assert_eq!(unconfirmed, ModelReasoningCapability::Unconfirmed);
        assert!(!unconfirmed.allows(ReasoningMode::None));
        assert_eq!(unconfirmed.wire(), None);
    }

    #[test]
    fn wire_controls_clear_other_parameters_so_no_two_are_sent() {
        let mut parameters = serde_json::Map::new();
        parameters.insert("reasoning_effort".to_string(), serde_json::json!("none"));
        ReasoningWireControl::ThinkingDisabled.apply(&mut parameters);
        assert_eq!(parameters.get("thinking"), Some(&serde_json::json!({"type": "disabled"})));
        assert!(
            parameters.get("reasoning_effort").is_none(),
            "the automatic reasoning_effort must be cleared before a thinking control is sent"
        );

        let mut parameters = serde_json::Map::new();
        parameters.insert("thinking".to_string(), serde_json::json!({"type": "enabled"}));
        ReasoningWireControl::ReasoningEffortNone.apply(&mut parameters);
        assert_eq!(parameters.get("reasoning_effort"), Some(&serde_json::json!("none")));
        assert!(parameters.get("thinking").is_none());
    }

    #[test]
    fn raw_parameters_must_agree_with_the_confirmed_wire_control() {
        let disagreeing = serde_json::json!({"reasoning_effort": "high"});
        assert!(ReasoningWireControl::ReasoningEffortNone.raw_conflict(&disagreeing).is_some());
        let agreeing = serde_json::json!({"reasoning_effort": "none"});
        assert!(ReasoningWireControl::ReasoningEffortNone.raw_conflict(&agreeing).is_none());

        let thinking_on = serde_json::json!({"thinking": {"type": "enabled"}});
        assert!(ReasoningWireControl::ThinkingDisabled.raw_conflict(&thinking_on).is_some());
        let thinking_off = serde_json::json!({"thinking": {"type": "disabled"}});
        assert!(ReasoningWireControl::ThinkingDisabled.raw_conflict(&thinking_off).is_none());
        assert!(ReasoningWireControl::ReasoningEffortNone.raw_conflict(&thinking_on).is_some());
        assert!(ReasoningWireControl::ThinkingDisabled.raw_conflict(&disagreeing).is_some());
        // A second switch is not the declared wire form, even if it also says off.
        assert!(ReasoningWireControl::ReasoningEffortNone.raw_conflict(&thinking_off).is_some());
        assert!(ReasoningWireControl::ThinkingDisabled.raw_conflict(&agreeing).is_some());
    }

    #[test]
    fn unattributed_unknown_or_undocumented_confirmation_is_rejected() {
        for malformed in [
            serde_json::json!({"confirmed": {"mode": "none", "wire": "reasoning_effort_none"}}),
            serde_json::json!({"confirmed": {"mode": "none", "wire": "reasoning_effort_none", "evidence": "   "}}),
            serde_json::json!({"confirmed": {"mode": "turbo", "wire": "reasoning_effort_none", "evidence": "docs"}}),
            serde_json::json!({"confirmed": {"mode": "none", "wire": "guess_from_name", "evidence": "docs"}}),
        ] {
            let metadata = serde_json::json!({ "reasoningCapability": malformed.clone() });
            assert!(model_reasoning_capability(&metadata).is_err(), "{malformed}");
        }

        assert!(
            confirmed_capability("none", "thinking_disabled", DEEPSEEK_EVIDENCE).validate().is_ok()
        );
        assert!(ModelReasoningCapability::Unconfirmed.validate().is_ok());
    }

    #[test]
    fn provider_request_policy_cannot_carry_a_pair_capability() {
        // The capability is model-owned, not provider-wide: a provider request
        // policy that tries to carry it is an unknown field and fails to parse,
        // which is what stops a profile grant from authorizing a model.
        let parsed = serde_json::from_value::<super::ProviderRequestPolicy>(serde_json::json!({
            "reasoningCapability": {"confirmed": {"mode": "none", "wire": "reasoning_effort_none", "evidence": "docs"}}
        }));
        assert!(parsed.is_err(), "a provider-wide capability must not parse");
    }

    #[test]
    fn unconfirmed_capability_rejects_a_mode() {
        assert!(!ModelReasoningCapability::Unconfirmed.allows(ReasoningMode::None));
        assert!(ModelReasoningCapability::Unconfirmed.validate().is_ok());
    }
}
