use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use rust_decimal::Decimal;

pub(crate) const OPENAI_COMPATIBLE_RUNTIME_KIND: &str = "openai_compatible";
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
    /// Binding purposes no seeded model of this provider covers, each with the
    /// reason a person can act on. Setup shows these next to the provider so an
    /// operator resolves the gap instead of discovering it when their first
    /// document fails to ingest.
    #[serde(default)]
    pub purpose_gaps: Vec<ProviderPurposeGap>,
    #[serde(default)]
    pub ui_hints: serde_json::Value,
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
