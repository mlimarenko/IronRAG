//! Clean-v1 AI provider catalog seeded by startup authority after migrations.
//!
//! Catalog identities are UUID5 values so re-running startup is deterministic.
//! The seed owns the model rows it writes, marked in `metadata_json`; it
//! retires its own rows that a later manifest drops and leaves operator-created
//! and runtime-discovered rows alone. A router's chat set is seeded from one
//! listing and may lag it, so the binding screen still reads the live listing
//! for what the account reaches now. Pricing is part of the model row itself:
//! every seeded model carries its current vendor rates, and startup refreshes
//! them.
//!
//! Vendor sources behind every seeded rate, fetched 2026-09-17:
//!   OpenAI     <https://platform.openai.com/docs/pricing> (+ /docs/models)
//!   Anthropic  <https://platform.claude.com/docs/en/about-claude/pricing>
//!   Gemini     <https://ai.google.dev/gemini-api/docs/pricing> (+ /docs/models)
//!   DeepSeek   <https://api-docs.deepseek.com/quick_start/pricing>
//!   MiniMax    <https://platform.minimax.io/docs/guides/pricing-paygo.md>
//!   Ollama     <https://ollama.com/pricing> (+ /library)
//!   Qwen       <https://www.alibabacloud.com/pricing> (Model Studio, page updated 2026-09-15)
//!   OpenRouter <https://openrouter.ai/api/v1/models>
//!   RouterAI   <https://routerai.ru/api/v1/models>
//!   LiteLLM    <https://models.litellm.ai>
//! GPTunnel publishes no authless price list (`/v1/models` 401, `api.gptunnel.ru/v1/models`
//! 404, HTML routes are a JS-only shell), so it seeds no models and its gap is
//! recorded in the provider entry. A rate absent from a source stays null with a
//! comment; it is never inferred from a model id or a name.

use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

const SEED_NAMESPACE: Uuid = Uuid::NAMESPACE_DNS;

/// `metadata_json` key that marks a row as written by this seed. A later
/// startup retires only rows carrying it; the operator's own catalog rows and
/// the models runtime discovery creates must survive an upgrade untouched.
const SEED_SOURCE_METADATA_KEY: &str = "seedSource";
const SEED_SOURCE_CLEAN_V1: &str = "clean_v1";

#[derive(Clone, Copy)]
struct ProviderSeed {
    kind: &'static str,
    display_name: &'static str,
    api_style: &'static str,
    default_base_url: Option<&'static str>,
    profile: ProfileSeed,
    bootstrap_presets: &'static [BootstrapPreset],
    models: &'static [ModelSeed],
    /// Purposes no seeded model of this provider covers, each with the reason.
    /// A purpose is either served by a seeded model or declared here — the
    /// catalog gate accepts nothing in between, because the alternative is what
    /// shipped before it: a deployment configures five purposes out of six and
    /// learns about the sixth when its first document fails to ingest.
    purpose_gaps: &'static [PurposeGap],
    /// `ai_provider_lifecycle_state` this provider seeds as. Every existing
    /// provider ships `active`; TypeSafe Jev (IRG-182) is the first `preview`
    /// entry — an opt-in decision provider with no credential and no binding,
    /// so it never affects an existing deployment's runtime.
    lifecycle_state: &'static str,
}

#[derive(Clone, Copy)]
struct PurposeGap {
    purpose: BindingPurpose,
    /// Whether the provider is incapable, or merely un-seeded. Collapsing the
    /// two is how a router came to be labelled "not supported" for chat, which
    /// it serves perfectly well once the operator picks a model.
    kind: PurposeGapKind,
    /// Stable machine-readable code. A UI translates it; the English `reason`
    /// below is the fallback for API consumers that have no catalogue of their
    /// own. Sending only prose meant a Russian screen rendered English.
    code: &'static str,
    reason: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PurposeGapKind {
    /// The provider cannot serve the purpose at all: no endpoint for it, no
    /// vision, no schema-constrained output.
    ProviderCannot,
    /// The provider can serve it, but which model does so is the operator's
    /// choice — a router proxies other vendors' catalogues and a local runtime
    /// serves whatever was pulled. `GET /v1/ai/accounts/{id}/available-models`
    /// reads that list live from the account.
    OperatorSupplies,
}

impl PurposeGapKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderCannot => "provider_cannot",
            Self::OperatorSupplies => "operator_supplies",
        }
    }
}

/// The native runtimes answer with prose, not a schema-constrained object
/// (`structuredOutput: "unsupported"` in their profile), and graph extraction
/// consumes a typed object. This is a transport limitation, not a judgement
/// about the model.
const NATIVE_RUNTIME_HAS_NO_STRUCTURED_OUTPUT: &str =
    "native runtime returns prose, and graph extraction needs schema-constrained output";

impl ProviderSeed {
    const fn hosted_openai_compatible(
        kind: &'static str,
        display_name: &'static str,
        default_base_url: &'static str,
        models: &'static [ModelSeed],
        listed_pricing_currency: Option<&'static str>,
        purpose_gaps: &'static [PurposeGap],
    ) -> Self {
        Self {
            kind,
            display_name,
            api_style: "openai_compatible",
            default_base_url: Some(default_base_url),
            profile: ProfileSeed::OpenAiCompatible(OpenAiProfileSeed {
                listed_pricing_currency,
                ..COMPATIBLE_SEED
            }),
            bootstrap_presets: &[],
            models,
            purpose_gaps,
            lifecycle_state: "active",
        }
    }

    /// Currency the provider's listing quotes, defaulting to USD for the
    /// native vendors. `upsert_model` writes this onto every row, so a
    /// rouble-priced router is never recorded as dollars.
    const fn listed_pricing_currency(&self) -> &'static str {
        match self.profile {
            ProfileSeed::OpenAiCompatible(profile) => match profile.listed_pricing_currency {
                Some(currency) => currency,
                None => "USD",
            },
            ProfileSeed::Native(_) | ProfileSeed::Decision(_) => "USD",
        }
    }
}

#[derive(Clone, Copy)]
struct ModelSeed {
    model_name: &'static str,
    observed_input_modalities: &'static [&'static str],
    context_window: Option<i32>,
    max_output_tokens: Option<i32>,
    embedding_dimensions: Option<i32>,
    binding_purposes: &'static [BindingPurpose],
    pricing: PricingSeed,
    /// `ai_model_lifecycle_state` this model seeds as. Every constructor
    /// below seeds `active`; `jev-latest` (IRG-182) is built from a raw
    /// literal carrying `preview` instead.
    lifecycle_state: &'static str,
}

impl ModelSeed {
    const fn chat(
        model_name: &'static str,
        observed_input_modalities: &'static [&'static str],
        context_window: Option<i32>,
        max_output_tokens: Option<i32>,
        binding_purposes: &'static [BindingPurpose],
        pricing: PricingSeed,
    ) -> Self {
        Self {
            model_name,
            observed_input_modalities,
            context_window,
            max_output_tokens,
            embedding_dimensions: None,
            binding_purposes,
            pricing,
            lifecycle_state: "active",
        }
    }

    const fn native_multimodal_chat(
        model_name: &'static str,
        context_window: i32,
        max_output_tokens: i32,
        pricing: PricingSeed,
    ) -> Self {
        Self::chat(
            model_name,
            TEXT_AND_IMAGE,
            Some(context_window),
            Some(max_output_tokens),
            NATIVE_MULTIMODAL_PURPOSES,
            pricing,
        )
    }

    const fn embedding(
        model_name: &'static str,
        embedding_dimensions: i32,
        binding_purposes: &'static [BindingPurpose],
        input_price_per_1m: &'static str,
    ) -> Self {
        Self {
            model_name,
            observed_input_modalities: TEXT,
            context_window: None,
            max_output_tokens: None,
            embedding_dimensions: Some(embedding_dimensions),
            binding_purposes,
            pricing: PricingSeed::embedding(input_price_per_1m),
            lifecycle_state: "active",
        }
    }
}

#[derive(Clone, Copy)]
struct BootstrapPreset {
    purpose: BindingPurpose,
    model_name: &'static str,
    /// Request parameters the preset binding is created with, for example the
    /// reasoning effort or the provider's service tier. They become the
    /// binding's `extraParametersJson`; an operator edit wins on replay.
    parameters: &'static [(&'static str, &'static str)],
}

const fn preset(purpose: BindingPurpose, model_name: &'static str) -> BootstrapPreset {
    BootstrapPreset { purpose, model_name, parameters: &[] }
}

const fn preset_with(
    purpose: BindingPurpose,
    model_name: &'static str,
    parameters: &'static [(&'static str, &'static str)],
) -> BootstrapPreset {
    BootstrapPreset { purpose, model_name, parameters }
}

/// Vendor rates carried by every seeded model. Zero is an honest price
/// (local models, embedding output); "no price" is unrepresentable.
#[derive(Clone, Copy)]
struct PricingSeed {
    input: &'static str,
    cached_input: &'static str,
    cache_write_input: &'static str,
    output: &'static str,
    long_context: Option<LongContextSeed>,
}

#[derive(Clone, Copy)]
struct LongContextSeed {
    threshold_tokens: i32,
    input: &'static str,
    cached_input: &'static str,
    output: &'static str,
}

impl PricingSeed {
    const fn flat(input: &'static str, output: &'static str) -> Self {
        Self { input, cached_input: "0", cache_write_input: "0", output, long_context: None }
    }

    const fn with_cached(
        input: &'static str,
        cached_input: &'static str,
        output: &'static str,
    ) -> Self {
        Self { input, cached_input, cache_write_input: "0", output, long_context: None }
    }

    const fn embedding(input: &'static str) -> Self {
        Self { input, cached_input: "0", cache_write_input: "0", output: "0", long_context: None }
    }
}

#[derive(Clone, Copy)]
struct ModelRow {
    provider: &'static ProviderSeed,
    model: &'static ModelSeed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingPurpose {
    ExtractText,
    ExtractGraph,
    GraphEntityType,
    EmbedChunk,
    QueryCompile,
    QueryRerank,
    QueryAnswer,
    Agent,
}

impl BindingPurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExtractText => "extract_text",
            Self::ExtractGraph => "extract_graph",
            Self::GraphEntityType => "graph_entity_type",
            Self::EmbedChunk => "embed_chunk",
            Self::QueryCompile => "query_compile",
            Self::QueryRerank => "query_rerank",
            Self::QueryAnswer => "query_answer",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Copy)]
enum ProfileSeed {
    Native(NativeProfileSeed),
    OpenAiCompatible(OpenAiProfileSeed),
    /// A typed decision endpoint (TypeSafe System One, IRG-182): every
    /// generative capability is `unsupported`, every discovery path declares
    /// `capabilityKind: decision`, and `capabilities.decision` is the only
    /// one `supported` — the invariant `validate_decision_capability_invariant`
    /// enforces on every provider parse.
    Decision(DecisionProfileSeed),
}

/// A typed decision endpoint's seed shape. Deliberately narrow: unlike
/// `OpenAiProfileSeed` a decision provider has no chat/embeddings request
/// shape to vary — `chat_path` is filler the gateway never dispatches to
/// (`resolve_runtime_profile` refuses any `runtime.kind` it does not
/// recognise, and `typesafe_system_one` is not one of the three it knows),
/// carried only so the profile has a well-formed `runtime.chatPath`.
#[derive(Clone, Copy)]
struct DecisionProfileSeed {
    runtime_kind: &'static str,
    chat_path: &'static str,
    models_path: &'static str,
    score_criteria: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct NativeProfileSeed {
    runtime_kind: &'static str,
    auth_scheme: &'static str,
    chat_path: &'static str,
    models_path: &'static str,
}

#[derive(Clone, Copy)]
struct OpenAiProfileSeed {
    base_url_mode: BaseUrlMode,
    api_key_required: bool,
    model_discovery: bool,
    embedding_discovery: bool,
    allow_private_network: bool,
    native_capabilities: bool,
    token_limit_parameter: &'static str,
    structured_output: &'static str,
    /// Whether the endpoint accepts the half-price `service_tier: "flex"`
    /// (published as `capabilities.flexTier`).
    flex_tier: bool,
    /// Price multipliers the provider applies per actual response
    /// `service_tier`, published as `profile.serviceTierMultipliers`. Empty
    /// where the endpoint publishes no tiered pricing. OpenAI prices `flex` at
    /// half the standard rate and `priority` at double
    /// (<https://platform.openai.com/docs/pricing>).
    service_tier_multipliers: &'static [(&'static str, &'static str)],
    /// Whether saving an account is checked against the provider by listing
    /// its models. Discovery and validation used to share one flag, which made
    /// "the binding screen can list your models" also mean "you cannot save an
    /// account until the runtime answers" — wrong for a local runtime that is
    /// often not up yet when its account is created.
    validate_credentials_on_save: bool,
    /// Currency the provider's `/models` listing quotes rates in. The listing
    /// itself carries a bare number: a rouble-priced router fills the same
    /// field a dollar-priced one does. `None` where the unit is not known, and
    /// a discovered model then carries no price rather than a wrong one.
    listed_pricing_currency: Option<&'static str>,
    /// Native OpenAI reasoning models accept only the default sampling values and
    /// reject an explicit `temperature`/`top_p` with HTTP 400, so the gateway must
    /// omit them. OpenAI-compatible routers front arbitrary models and keep
    /// forwarding the caller's sampling choice.
    sampling: &'static str,
    /// Typed `requestPolicy.syntheticToolHistory`: whether the endpoint
    /// accepts an assistant tool-call record that no model turn produced.
    /// DeepSeek's Chat Completions API rejects tool calls inserted into the
    /// middle of a conversation, so its fixed-context stages carry the
    /// evidence inside the last user message instead.
    synthetic_tool_history: &'static str,
    /// Typed `requestPolicy.reasoning`: whether the endpoint accepts the
    /// formal `reasoning_effort` control, so a caller with an answer-only
    /// completion budget can switch hidden reasoning off. Routers front
    /// arbitrary models and have no verified control, so they forward the
    /// binding's own parameters.
    reasoning: &'static str,
    /// Typed `requestPolicy.toolChoice`: whether the endpoint accepts a forced
    /// `tool_choice`. DeepSeek rejects `required` and named choices in its
    /// default thinking mode with HTTP 400, so it declares `auto_only` and the
    /// loop's server-side fallback call remains the deterministic first step.
    tool_choice: &'static str,
}

#[derive(Clone, Copy)]
enum BaseUrlMode {
    Fixed,
    Required,
}

impl BaseUrlMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Required => "required",
        }
    }

    const fn is_fixed(self) -> bool {
        matches!(self, Self::Fixed)
    }
}

const TEXT: &[&str] = &["text"];
const TEXT_AND_IMAGE: &[&str] = &["text", "image"];
const NATIVE_MULTIMODAL_PURPOSES: &[BindingPurpose] = &[
    BindingPurpose::ExtractText,
    BindingPurpose::QueryCompile,
    BindingPurpose::QueryRerank,
    BindingPurpose::GraphEntityType,
    BindingPurpose::QueryAnswer,
    BindingPurpose::Agent,
];
const OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES: &[BindingPurpose] = &[
    BindingPurpose::ExtractText,
    BindingPurpose::ExtractGraph,
    BindingPurpose::QueryCompile,
    BindingPurpose::QueryRerank,
    BindingPurpose::GraphEntityType,
    BindingPurpose::QueryAnswer,
    BindingPurpose::Agent,
];
const OPENAI_COMPATIBLE_TEXT_PURPOSES: &[BindingPurpose] = &[
    BindingPurpose::ExtractGraph,
    BindingPurpose::QueryCompile,
    BindingPurpose::QueryRerank,
    BindingPurpose::GraphEntityType,
    BindingPurpose::QueryAnswer,
    BindingPurpose::Agent,
];
/// Background ingest work tolerates Flex latency and pays half the standard
/// rate for it (<https://platform.openai.com/docs/pricing>).
const OPENAI_FLEX: (&str, &str) = ("service_tier", "flex");
/// Classification-shaped work gains nothing from reasoning (IRG-184, IRG-207).
const NO_REASONING: (&str, &str) = ("reasoning_effort", "none");

// GPT-6 Luna serves every chat purpose: on the IRG-184/IRG-207 benchmarks it
// matches or beats the 5.4/5.6 tiers on typing and graph agreement at the
// lowest price. Ingest purposes run on Flex; interactive purposes stay on the
// standard tier for latency.
const OPENAI_BOOTSTRAP_PRESETS: &[BootstrapPreset] = &[
    preset_with(BindingPurpose::ExtractText, "gpt-6-luna", &[OPENAI_FLEX]),
    preset_with(BindingPurpose::ExtractGraph, "gpt-6-luna", &[NO_REASONING, OPENAI_FLEX]),
    preset(BindingPurpose::EmbedChunk, "text-embedding-3-large"),
    preset(BindingPurpose::QueryCompile, "gpt-6-luna"),
    preset_with(BindingPurpose::QueryRerank, "gpt-6-luna", &[NO_REASONING]),
    preset_with(BindingPurpose::GraphEntityType, "gpt-6-luna", &[NO_REASONING, OPENAI_FLEX]),
    preset(BindingPurpose::QueryAnswer, "gpt-6-luna"),
    preset(BindingPurpose::Agent, "gpt-6-luna"),
];

// Haiku carries the cheap text purposes and Opus/Sonnet the answer and agent
// work; every model in the seeded set can serve every native purpose.
const ANTHROPIC_BOOTSTRAP_PRESETS: &[BootstrapPreset] = &[
    preset(BindingPurpose::ExtractText, "claude-haiku-4-5-20251001"),
    preset(BindingPurpose::QueryCompile, "claude-haiku-4-5-20251001"),
    preset(BindingPurpose::QueryRerank, "claude-haiku-4-5-20251001"),
    preset(BindingPurpose::GraphEntityType, "claude-haiku-4-5-20251001"),
    preset(BindingPurpose::QueryAnswer, "claude-opus-5"),
    preset(BindingPurpose::Agent, "claude-sonnet-5"),
];

const MINIMAX_BOOTSTRAP_PRESETS: &[BootstrapPreset] = &[
    preset(BindingPurpose::ExtractText, "MiniMax-M3"),
    preset(BindingPurpose::ExtractGraph, "MiniMax-M2.7"),
    preset(BindingPurpose::QueryCompile, "MiniMax-M2.7"),
    preset(BindingPurpose::QueryRerank, "MiniMax-M2.7"),
    preset(BindingPurpose::GraphEntityType, "MiniMax-M2.7"),
    preset(BindingPurpose::QueryAnswer, "MiniMax-M3"),
    preset(BindingPurpose::Agent, "MiniMax-M3"),
];

const DEEPSEEK_BOOTSTRAP_PRESETS: &[BootstrapPreset] = &[
    preset(BindingPurpose::ExtractText, "deepseek-flash"),
    preset(BindingPurpose::ExtractGraph, "deepseek-flash"),
    preset(BindingPurpose::QueryCompile, "deepseek-flash"),
    preset(BindingPurpose::QueryRerank, "deepseek-flash"),
    preset(BindingPurpose::GraphEntityType, "deepseek-flash"),
    preset(BindingPurpose::QueryAnswer, "deepseek-v4-pro"),
    preset(BindingPurpose::Agent, "deepseek-v4-pro"),
];

/// One GPT-6 model: every tier shares a 1.05M-token context, a 128K output
/// ceiling, text and image input, and a long-context tier above 272K input
/// tokens. `short` is input, cached input, cache write and output; `long` is
/// input, cached input and output of the long-context tier.
const fn gpt6_chat(
    model_name: &'static str,
    short: [&'static str; 4],
    long: [&'static str; 3],
) -> ModelSeed {
    ModelSeed::chat(
        model_name,
        TEXT_AND_IMAGE,
        Some(1_050_000),
        Some(128_000),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: short[0],
            cached_input: short[1],
            cache_write_input: short[2],
            output: short[3],
            long_context: Some(LongContextSeed {
                threshold_tokens: 272_000,
                input: long[0],
                cached_input: long[1],
                output: long[2],
            }),
        },
    )
}

// Vendor rates as of 2026-09-17 (standard sync API, USD per 1M tokens), read
// from the pricing table at https://platform.openai.com/docs/pricing. The page
// prints short- and long-context columns separately, cached input included, so
// every number below is published rather than derived: the 5.6 family bills
// cache writes at 1.25x input, and above the 272K-token long-context threshold
// input doubles and output rises by half. The 5.4 small tiers have no
// long-context tier and no billed cache writes.
const OPENAI_MODELS: &[ModelSeed] = &[
    // Astra, the current frontier tier.
    gpt6_chat("gpt-6-astra", ["10", "1", "12.5", "50"], ["20", "2", "75"]),
    // Sol and Luna, the GPT-6 mid and efficient tiers (published 2026-09-27).
    gpt6_chat("gpt-6-sol", ["2", "0.2", "2.5", "10"], ["4", "0.4", "15"]),
    gpt6_chat("gpt-6-luna", ["0.1", "0.01", "0.125", "0.5"], ["0.2", "0.02", "0.75"]),
    // Sol's promotional input rate runs to 2026-11-21; the seed carries the
    // promotional price because that is what the account is billed today.
    ModelSeed::chat(
        "gpt-5.6-sol",
        TEXT_AND_IMAGE,
        Some(1_050_000),
        Some(128_000),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "4",
            cached_input: "0.4",
            cache_write_input: "5",
            output: "20",
            long_context: Some(LongContextSeed {
                threshold_tokens: 272_000,
                input: "8",
                cached_input: "0.8",
                output: "30",
            }),
        },
    ),
    ModelSeed::chat(
        "gpt-5.6-terra",
        TEXT_AND_IMAGE,
        Some(1_050_000),
        Some(128_000),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "2",
            cached_input: "0.2",
            cache_write_input: "2.5",
            output: "12",
            long_context: Some(LongContextSeed {
                threshold_tokens: 272_000,
                input: "4",
                cached_input: "0.4",
                output: "18",
            }),
        },
    ),
    ModelSeed::chat(
        "gpt-5.6-luna",
        TEXT_AND_IMAGE,
        Some(1_050_000),
        Some(128_000),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "0.2",
            cached_input: "0.02",
            cache_write_input: "0.25",
            output: "1.2",
            long_context: Some(LongContextSeed {
                threshold_tokens: 272_000,
                input: "0.4",
                cached_input: "0.04",
                output: "1.8",
            }),
        },
    ),
    // Cyber bills at the frontier rate with no long-context tier. The vendor
    // publishes no context ceiling for it, so the seed records none rather
    // than inventing one.
    ModelSeed::chat(
        "gpt-5.6-cyber",
        TEXT_AND_IMAGE,
        None,
        None,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "12.5",
            cached_input: "1.25",
            cache_write_input: "15.625",
            output: "75",
            long_context: None,
        },
    ),
    ModelSeed::chat(
        "gpt-5.4-mini",
        TEXT_AND_IMAGE,
        None,
        None,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed::with_cached("0.75", "0.075", "4.5"),
    ),
    ModelSeed::chat(
        "gpt-5.4-nano",
        TEXT_AND_IMAGE,
        None,
        None,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed::with_cached("0.2", "0.02", "1.25"),
    ),
    ModelSeed::embedding("text-embedding-3-large", 3072, &[BindingPurpose::EmbedChunk], "0.13"),
    ModelSeed::embedding("text-embedding-3-small", 1536, &[BindingPurpose::EmbedChunk], "0.02"),
];

// Anthropic bills prompt-cache reads at 0.1x input and cache writes at
// 1.25x input across the family, except Fable 5.1 whose cache read is 0.025x.
// The vendor also offers a 1-hour cache write at 2x input; the catalog carries
// a single cache-write rate, so it records the 5-minute one.
const fn anthropic_pricing(
    input: &'static str,
    cached_input: &'static str,
    cache_write_input: &'static str,
    output: &'static str,
) -> PricingSeed {
    PricingSeed { input, cached_input, cache_write_input, output, long_context: None }
}

const FABLE_PRICES: PricingSeed = anthropic_pricing("10", "0.25", "12.5", "50");
const OPUS_PRICES: PricingSeed = anthropic_pricing("5", "0.5", "6.25", "25");
const SONNET_PRICES: PricingSeed = anthropic_pricing("2", "0.2", "2.5", "10");
const HAIKU_PRICES: PricingSeed = anthropic_pricing("1", "0.1", "1.25", "5");

const ANTHROPIC_MODELS: &[ModelSeed] = &[
    ModelSeed::native_multimodal_chat("claude-fable-5-1", 1_000_000, 128_000, FABLE_PRICES),
    ModelSeed::native_multimodal_chat("claude-opus-5", 1_000_000, 128_000, OPUS_PRICES),
    ModelSeed::native_multimodal_chat("claude-sonnet-5", 1_000_000, 128_000, SONNET_PRICES),
    ModelSeed::native_multimodal_chat("claude-haiku-4-5-20251001", 200_000, 64_000, HAIKU_PRICES),
];

// Direct Gemini API models, read 2026-09-17 from
// https://ai.google.dev/gemini-api/docs/pricing and the per-model pages under
// https://ai.google.dev/gemini-api/docs/models, whose model list is the source
// of the ids. The paid-tier standard rates are seed data: Flash 3.8 is on a
// promotional input/output rate through 2026-12-31 (the seed carries what the
// account is billed today), and Pro 3.1 bills more above a 200K-token prompt.
// The per-model pages publish the 1,048,576 input / 65,536 output token limits.
const GEMINI_MODELS: &[ModelSeed] = &[
    ModelSeed::chat(
        "gemini-3.1-pro-preview",
        TEXT_AND_IMAGE,
        Some(1_048_576),
        Some(65_536),
        NATIVE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "2",
            cached_input: "0.2",
            cache_write_input: "0",
            output: "12",
            long_context: Some(LongContextSeed {
                threshold_tokens: 200_000,
                input: "4",
                cached_input: "0.4",
                output: "18",
            }),
        },
    ),
    ModelSeed::chat(
        "gemini-3.8-flash",
        TEXT_AND_IMAGE,
        Some(1_048_576),
        Some(65_536),
        NATIVE_MULTIMODAL_PURPOSES,
        PricingSeed::with_cached("0.75", "0.075", "3.75"),
    ),
    ModelSeed::chat(
        "gemini-3.5-flash-lite",
        TEXT_AND_IMAGE,
        Some(1_048_576),
        Some(65_536),
        NATIVE_MULTIMODAL_PURPOSES,
        PricingSeed::with_cached("0.3", "0.03", "2.5"),
    ),
    // Matryoshka model: 3072 is the native output, and 1536/768 are truncations
    // of it. The catalog seeds the native width; a deployment that wants a
    // narrower vector adds the model itself rather than getting a silently
    // truncated one. `gemini-embedding-2` is the multimodal successor with
    // configurable 128–3072 dimensions (recommended 768/1536/3072); it is
    // seeded at the native width, priced from the online tier ($0.20/1M, batch
    // $0.10). Model 001 stays: it is still Stable for text-only workloads.
    ModelSeed::embedding("gemini-embedding-001", 3072, &[BindingPurpose::EmbedChunk], "0.15"),
    ModelSeed::embedding("gemini-embedding-2", 3072, &[BindingPurpose::EmbedChunk], "0.20"),
];

const GEMINI_BOOTSTRAP_PRESETS: &[BootstrapPreset] = &[
    preset(BindingPurpose::ExtractText, "gemini-3.8-flash"),
    preset(BindingPurpose::EmbedChunk, "gemini-embedding-001"),
    preset(BindingPurpose::QueryCompile, "gemini-3.8-flash"),
    preset(BindingPurpose::QueryRerank, "gemini-3.8-flash"),
    preset(BindingPurpose::GraphEntityType, "gemini-3.8-flash"),
    preset(BindingPurpose::QueryAnswer, "gemini-3.1-pro-preview"),
    preset(BindingPurpose::Agent, "gemini-3.1-pro-preview"),
];

const MINIMAX_MODELS: &[ModelSeed] = &[
    ModelSeed::chat(
        "MiniMax-M3",
        TEXT_AND_IMAGE,
        Some(1_048_576),
        Some(262_144),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed {
            input: "0.3",
            cached_input: "0.06",
            cache_write_input: "0",
            output: "1.2",
            long_context: Some(LongContextSeed {
                threshold_tokens: 512_000,
                input: "0.6",
                cached_input: "0.12",
                output: "2.4",
            }),
        },
    ),
    // M2.7 bills cache reads at 0.06 and cache writes at 0.375; highspeed
    // doubles the base input/output rates but shares the cache rates.
    ModelSeed::chat(
        "MiniMax-M2.7",
        TEXT,
        Some(204_800),
        Some(196_608),
        OPENAI_COMPATIBLE_TEXT_PURPOSES,
        PricingSeed {
            input: "0.3",
            cached_input: "0.06",
            cache_write_input: "0.375",
            output: "1.2",
            long_context: None,
        },
    ),
    ModelSeed::chat(
        "MiniMax-M2.7-highspeed",
        TEXT,
        Some(204_800),
        None,
        OPENAI_COMPATIBLE_TEXT_PURPOSES,
        PricingSeed {
            input: "0.6",
            cached_input: "0.06",
            cache_write_input: "0.375",
            output: "2.4",
            long_context: None,
        },
    ),
];

// DeepSeek reprices by time of day (peak 01:00-04:00 and 06:00-10:00 UTC costs
// double off-peak), which a single rate cannot express. The seed carries the
// PEAK rate: a cost estimate that is too high is a smaller problem than one
// that quietly understates the bill. Rates are the 2026-09-17 pricing.
const DEEPSEEK_MODELS: &[ModelSeed] = &[
    ModelSeed::chat(
        "deepseek-v4-pro",
        TEXT,
        Some(1_000_000),
        Some(384_000),
        OPENAI_COMPATIBLE_TEXT_PURPOSES,
        PricingSeed::with_cached("1.32", "0.044", "3.96"),
    ),
    // V4.1-Flash is the vision-capable member of the family: it accepts
    // images, so it serves document understanding as well as graph
    // extraction (owner, 2026-09-17; OpenRouter metadata agrees). V4-Pro
    // stays text-only.
    ModelSeed::chat(
        "deepseek-flash",
        TEXT_AND_IMAGE,
        Some(1_000_000),
        Some(384_000),
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        PricingSeed::with_cached("0.3", "0.006", "1.2"),
    ),
];

// Ollama serves models the operator pulled locally, so the catalog cannot
// enumerate its chat models. Its embedding models are the exception worth
// seeding: they are the two the ecosystem actually standardised on, and
// without at least one embedding model a local-only deployment cannot ingest
// anything at all. Zero is the honest price for a model running on your own
// hardware.
/// GPTunnel publishes no authless price list, so the catalog seeds none of its
/// models and declares every chat purpose operator-supplied; the setup screen
/// then says so instead of showing an empty model list with no explanation.
/// A router's chat set is seeded from one listing and may lag it, so the
/// binding screen still reads the live listing.
const ROUTER_OPERATOR_SUPPLIED: &[PurposeGap] = &[
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractText,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractGraph,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryCompile,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryRerank,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::GraphEntityType,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryAnswer,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::Agent,
        code: OPERATOR_SUPPLIED_CODE,
        reason: ROUTER_ADD_YOUR_MODELS,
    },
];

/// Ollama serves whatever the operator pulled, so the catalog can seed its
/// embedding models (the ecosystem standardised on two) but never its chat
/// models. Declaring each chat purpose lets setup say that, instead of
/// presenting a provider that looks broken.
const OLLAMA_OPERATOR_SUPPLIED: &[PurposeGap] = &[
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractText,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractGraph,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryCompile,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryRerank,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::GraphEntityType,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryAnswer,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::Agent,
        code: OPERATOR_SUPPLIED_CODE,
        reason: OLLAMA_PULL_YOUR_MODELS,
    },
];

const OLLAMA_PULL_YOUR_MODELS: &str =
    "chat models are whatever you pulled locally; the binding screen lists them from the runtime";

/// Both routers and Ollama expose whatever the operator configured, so a UI
/// shows one message for both.
const OPERATOR_SUPPLIED_CODE: &str = "operator_supplied_models";

const ROUTER_ADD_YOUR_MODELS: &str = "router chat catalogues change weekly; the binding screen lists what your key \
     actually exposes";

/// Qwen publishes priced chat and vision ids, but no verified embedding id and
/// rate, so its embedding purpose is the operator's to pick.
const QWEN_PURPOSE_GAPS: &[PurposeGap] = &[PurposeGap {
    kind: PurposeGapKind::OperatorSupplies,
    purpose: BindingPurpose::EmbedChunk,
    code: OPERATOR_SUPPLIED_CODE,
    reason: QWEN_EMBEDDING_OPERATOR_SUPPLIED,
}];

const QWEN_EMBEDDING_OPERATOR_SUPPLIED: &str =
    "no verified embedding id and price is published; pick one from the live listing";

/// A LiteLLM gateway fronts whatever the operator configured — chat and
/// embeddings alike — so every purpose is the operator's to supply from the
/// gateway's own `/models` listing.
const LITELLM_OPERATOR_SUPPLIED: &[PurposeGap] = &[
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractText,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::ExtractGraph,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::EmbedChunk,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryCompile,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryRerank,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::GraphEntityType,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::QueryAnswer,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
    PurposeGap {
        kind: PurposeGapKind::OperatorSupplies,
        purpose: BindingPurpose::Agent,
        code: OPERATOR_SUPPLIED_CODE,
        reason: LITELLM_OPERATOR_SUPPLIED_REASON,
    },
];

const LITELLM_OPERATOR_SUPPLIED_REASON: &str = "the gateway serves the models you configure; the binding screen lists what its /models exposes";

// Ollama's cloud chat is a separate hosted service reached through a different
// base URL and key, so it is not seeded here; this entry is the local runtime,
// whose chat models are whatever the operator pulled. Zero is the honest price
// for a model running on the operator's own hardware.
const OLLAMA_MODELS: &[ModelSeed] = &[
    ModelSeed::embedding("nomic-embed-text", 768, &[BindingPurpose::EmbedChunk], "0"),
    ModelSeed::embedding("mxbai-embed-large", 1024, &[BindingPurpose::EmbedChunk], "0"),
];

// Both routers list the same chat ids, so a shared constructor keeps the two
// manifests in step; only the currency and rates differ between them.
const fn router_chat(
    model_name: &'static str,
    context_window: i32,
    observed_input_modalities: &'static [&'static str],
    binding_purposes: &'static [BindingPurpose],
    input: &'static str,
    output: &'static str,
) -> ModelSeed {
    ModelSeed::chat(
        model_name,
        observed_input_modalities,
        Some(context_window),
        None,
        binding_purposes,
        PricingSeed::flat(input, output),
    )
}

// OpenRouter publishes its catalogue as public JSON with per-token US-dollar
// rates the router bills directly. These are the current chat ids with a
// verified upstream rate; cached input is zero because the listing carries no
// separate cached rate and the seed does not invent one. The two OpenAI
// embedding ids stay seeded so a router-only deployment can ingest.
const OPENROUTER_MODELS: &[ModelSeed] = &[
    router_chat(
        "openai/gpt-6-astra",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "10",
        "50",
    ),
    router_chat(
        "openai/gpt-6-sol",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "10",
    ),
    router_chat(
        "openai/gpt-6-luna",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.1",
        "0.5",
    ),
    router_chat(
        "openai/gpt-5.6-sol",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "10",
    ),
    router_chat(
        "openai/gpt-5.4-mini",
        400_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.75",
        "4.5",
    ),
    router_chat(
        "anthropic/claude-fable-5.1",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "10",
        "50",
    ),
    router_chat(
        "anthropic/claude-opus-5",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "5",
        "25",
    ),
    router_chat(
        "anthropic/claude-sonnet-5",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "10",
    ),
    router_chat(
        "anthropic/claude-haiku-4.5",
        200_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "1",
        "5",
    ),
    router_chat(
        "google/gemini-3.1-pro-preview",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "12",
    ),
    router_chat(
        "google/gemini-3.6-flash",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.75",
        "3.75",
    ),
    router_chat(
        "deepseek/deepseek-v4.1-flash",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.3",
        "1.2",
    ),
    router_chat(
        "deepseek/deepseek-v4-pro-0813",
        1_048_576,
        TEXT,
        OPENAI_COMPATIBLE_TEXT_PURPOSES,
        "1.32",
        "3.96",
    ),
    router_chat(
        "qwen/qwen3.8-max-0902",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "6",
    ),
    router_chat(
        "qwen/qwen3.8-flash",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.15",
        "0.47",
    ),
    router_chat(
        "minimax/minimax-m3",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.3",
        "1.2",
    ),
    router_chat(
        "x-ai/grok-4.6",
        500_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "6",
    ),
    router_chat(
        "mistralai/mistral-large-2512",
        262_144,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.5",
        "1.5",
    ),
    ModelSeed::embedding(
        "openai/text-embedding-3-large",
        3072,
        &[BindingPurpose::EmbedChunk],
        "0.13",
    ),
    ModelSeed::embedding(
        "openai/text-embedding-3-small",
        1536,
        &[BindingPurpose::EmbedChunk],
        "0.02",
    ),
];

// RouterAI lists the same ids with prices in roubles; the router bills its
// rouble listing directly. The rows are identical to OpenRouter's except for
// the rates, and the currency travels on the row so cost reporting is not off
// by the exchange rate. Its listing also prices the two OpenAI embedding ids,
// so unlike a chat-only router it serves every binding purpose and declares no
// gap.
const ROUTERAI_MODELS: &[ModelSeed] = &[
    router_chat(
        "openai/gpt-6-astra",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "1094.25",
        "5471.26",
    ),
    router_chat(
        "openai/gpt-6-sol",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "219.29",
        "1096.44",
    ),
    router_chat(
        "openai/gpt-6-luna",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "10.96",
        "54.82",
    ),
    router_chat(
        "openai/gpt-5.6-sol",
        1_050_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "218.85",
        "1094.25",
    ),
    router_chat(
        "openai/gpt-5.4-mini",
        400_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "82.07",
        "492.41",
    ),
    router_chat(
        "anthropic/claude-fable-5.1",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "1258.39",
        "6291.95",
    ),
    router_chat(
        "anthropic/claude-opus-5",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "547.13",
        "2735.63",
    ),
    router_chat(
        "anthropic/claude-sonnet-5",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "218.85",
        "1094.25",
    ),
    router_chat(
        "anthropic/claude-haiku-4.5",
        200_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "109.43",
        "547.13",
    ),
    router_chat(
        "google/gemini-3.1-pro-preview",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "218.85",
        "1313.10",
    ),
    router_chat(
        "google/gemini-3.6-flash",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "82.07",
        "410.34",
    ),
    router_chat(
        "deepseek/deepseek-v4.1-flash",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "18.06",
        "72.22",
    ),
    router_chat(
        "deepseek/deepseek-v4-pro-0813",
        1_048_576,
        TEXT,
        OPENAI_COMPATIBLE_TEXT_PURPOSES,
        "101.11",
        "303.33",
    ),
    router_chat(
        "qwen/qwen3.8-max-0902",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "218.85",
        "656.55",
    ),
    router_chat(
        "qwen/qwen3.8-flash",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "16.41",
        "51.43",
    ),
    router_chat(
        "minimax/minimax-m3",
        1_048_576,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "25.17",
        "105.05",
    ),
    router_chat(
        "x-ai/grok-4.6",
        500_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "218.85",
        "656.55",
    ),
    router_chat(
        "mistralai/mistral-large-2512",
        262_144,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "54.71",
        "164.14",
    ),
    // Roubles per 1M input tokens, live from the router's listing.
    ModelSeed::embedding(
        "openai/text-embedding-3-large",
        3072,
        &[BindingPurpose::EmbedChunk],
        "14.23",
    ),
    ModelSeed::embedding(
        "openai/text-embedding-3-small",
        1536,
        &[BindingPurpose::EmbedChunk],
        "2.19",
    ),
];

// Qwen's hosted OpenAI-compatible endpoint. Chat and vision ids are published
// with USD rates; no embedding id and rate was verified, so that purpose is
// declared operator-supplied. Context windows come from the vendor's per-tier
// limits; output ceilings are not published, so the rows record none.
const QWEN_MODELS: &[ModelSeed] = &[
    router_chat(
        "qwen3.8-max",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "2",
        "6",
    ),
    router_chat(
        "qwen3.8-flash",
        1_000_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.15",
        "0.47",
    ),
    router_chat("qwen3.7-plus", 256_000, TEXT, OPENAI_COMPATIBLE_TEXT_PURPOSES, "0.4", "1.6"),
    router_chat("qwen3.7-flash", 32_000, TEXT, OPENAI_COMPATIBLE_TEXT_PURPOSES, "0.030", "0.130"),
    router_chat("qwen3-coder-plus", 32_000, TEXT, OPENAI_COMPATIBLE_TEXT_PURPOSES, "1", "5"),
    router_chat("qwen3-coder-flash", 32_000, TEXT, OPENAI_COMPATIBLE_TEXT_PURPOSES, "0.3", "1.5"),
    router_chat(
        "qwen3-vl-plus",
        32_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.2",
        "1.6",
    ),
    router_chat(
        "qwen3-vl-flash",
        32_000,
        TEXT_AND_IMAGE,
        OPENAI_COMPATIBLE_MULTIMODAL_PURPOSES,
        "0.05",
        "0.4",
    ),
];

const OPENAI_PROFILE: ProfileSeed = ProfileSeed::OpenAiCompatible(OpenAiProfileSeed {
    flex_tier: true,
    service_tier_multipliers: &[("flex", "0.5"), ("priority", "2")],
    base_url_mode: BaseUrlMode::Fixed,
    api_key_required: true,
    model_discovery: true,
    embedding_discovery: true,
    allow_private_network: false,
    native_capabilities: true,
    token_limit_parameter: "max_completion_tokens",
    validate_credentials_on_save: true,
    listed_pricing_currency: None,
    structured_output: "json_schema",
    sampling: "omit",
    reasoning: "disable_with_none",
    synthetic_tool_history: "transcript",
    tool_choice: "required_capable",
});
const COMPATIBLE_SEED: OpenAiProfileSeed = OpenAiProfileSeed {
    flex_tier: false,
    service_tier_multipliers: &[],
    base_url_mode: BaseUrlMode::Fixed,
    api_key_required: true,
    model_discovery: true,
    embedding_discovery: false,
    allow_private_network: false,
    native_capabilities: false,
    token_limit_parameter: "max_completion_tokens",
    structured_output: "prompt_only_json_object",
    validate_credentials_on_save: true,
    listed_pricing_currency: None,
    sampling: "forward",
    reasoning: "forward",
    synthetic_tool_history: "transcript",
    tool_choice: "required_capable",
};
const COMPATIBLE_PROFILE: ProfileSeed = ProfileSeed::OpenAiCompatible(COMPATIBLE_SEED);
const DEEPSEEK_PROFILE: ProfileSeed = ProfileSeed::OpenAiCompatible(OpenAiProfileSeed {
    flex_tier: false,
    service_tier_multipliers: &[],
    base_url_mode: BaseUrlMode::Fixed,
    api_key_required: true,
    model_discovery: true,
    embedding_discovery: false,
    allow_private_network: false,
    native_capabilities: false,
    token_limit_parameter: "max_tokens",
    validate_credentials_on_save: true,
    listed_pricing_currency: None,
    structured_output: "json_object",
    sampling: "omit",
    // The endpoint accepts the formal `reasoning_effort` control
    // (none/low/high/max), so bounded extraction calls can switch the
    // default-on hidden reasoning off; answer calls keep it.
    reasoning: "disable_with_none",
    synthetic_tool_history: "inline",
    tool_choice: "auto_only",
});
// Ollama serves the OpenAI-compatible `/v1/models`, listing exactly what was
// pulled onto the host. Seeding it as undiscoverable was what left the binding
// screen with an empty dropdown and nothing to say about it.
const OLLAMA_PROFILE: ProfileSeed = ProfileSeed::OpenAiCompatible(OpenAiProfileSeed {
    flex_tier: false,
    service_tier_multipliers: &[],
    base_url_mode: BaseUrlMode::Required,
    api_key_required: false,
    model_discovery: true,
    embedding_discovery: false,
    allow_private_network: true,
    native_capabilities: false,
    token_limit_parameter: "max_completion_tokens",
    validate_credentials_on_save: false,
    listed_pricing_currency: None,
    structured_output: "prompt_only_json_object",
    sampling: "forward",
    reasoning: "forward",
    synthetic_tool_history: "transcript",
    tool_choice: "required_capable",
});

// LiteLLM is a self-hosted gateway: it has no fixed public base URL, carries an
// API key, and may sit on a private network or not be up yet when its account
// is created, so saving the account does not contact it — the binding screen
// still lists what its `/models` exposes. Prices come from the gateway's own
// listing, whose unit is unknown here, hence no listed currency.
const LITELLM_PROFILE: ProfileSeed = ProfileSeed::OpenAiCompatible(OpenAiProfileSeed {
    flex_tier: false,
    service_tier_multipliers: &[],
    base_url_mode: BaseUrlMode::Required,
    api_key_required: true,
    model_discovery: true,
    embedding_discovery: false,
    allow_private_network: true,
    native_capabilities: false,
    token_limit_parameter: "max_completion_tokens",
    validate_credentials_on_save: false,
    listed_pricing_currency: None,
    structured_output: "prompt_only_json_object",
    sampling: "forward",
    reasoning: "forward",
    synthetic_tool_history: "transcript",
    tool_choice: "required_capable",
});

// TypeSafe Jev: a typed decision endpoint, not a chat vendor (IRG-182). It
// carries no bootstrap preset, no account and no binding — an operator opts
// in explicitly. `jev-latest` allows exactly the two decision purposes, both
// live: `query_rerank` since IRG-187 (SystemOne `Score`) and
// `graph_entity_type` since IRG-185 (SystemOne `Choice` or a fast chat model).
// `/v1/models` is not documented by TypeSafe; it is this seed's assumption,
// carrying the OpenAI-shaped `{"data":[{"id":...}]}` listing the `ModelList`
// credential-validation path (see `apps/api/src/services/ai_catalog_service/provider_access.rs`)
// already expects from every other seeded provider.
const TYPESAFE_JEV_PURPOSES: &[BindingPurpose] =
    &[BindingPurpose::GraphEntityType, BindingPurpose::QueryRerank];

const TYPESAFE_MODELS: &[ModelSeed] = &[ModelSeed {
    model_name: "jev-latest",
    observed_input_modalities: TEXT,
    context_window: None,
    max_output_tokens: None,
    embedding_dimensions: None,
    binding_purposes: TYPESAFE_JEV_PURPOSES,
    // Unpriced: TypeSafe Jev publishes no per-token rate, and the schema
    // represents "no price" as zero (see `PricingSeed`'s own doc comment).
    pricing: PricingSeed::flat("0", "0"),
    lifecycle_state: "preview",
}];

const TYPESAFE_PROVIDER: ProviderSeed = ProviderSeed {
    kind: "typesafe",
    display_name: "TypeSafe Jev",
    api_style: "typesafe_system_one",
    default_base_url: Some("https://api.typesafe.ai"),
    profile: ProfileSeed::Decision(DecisionProfileSeed {
        runtime_kind: "typesafe_system_one",
        // Documented as the invocation path the decision runtimes call. The
        // generative gateway still does not dispatch it: `resolve_runtime_profile`
        // refuses any `runtime.kind` it does not recognise, and
        // `typesafe_system_one` is not one of the three it knows. IRG-187's
        // semantic rerank and IRG-185's entity typing dispatch on this kind
        // directly.
        chat_path: "/v1/systemone",
        models_path: "/v1/models",
        score_criteria: TYPESAFE_SCORE_CRITERIA,
    }),
    bootstrap_presets: &[],
    models: TYPESAFE_MODELS,
    purpose_gaps: &[
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::ExtractText,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::ExtractGraph,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::EmbedChunk,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::QueryCompile,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::QueryAnswer,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
        PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::Agent,
            code: "decision_provider_cannot",
            reason: TYPESAFE_DECISION_ONLY,
        },
    ],
    lifecycle_state: "preview",
};

/// Ordered relevance levels of the rerank `Score` question, lowest to highest.
/// The protocol accepts `2..=10`; five levels normalise to the `[0, 1]` score
/// contract with one divisor. Catalog policy, read by the decision rerank from
/// the resolved provider profile — never a table in query code.
const TYPESAFE_SCORE_CRITERIA: &[&str] = &[
    "not relevant to the query",
    "weakly relevant to the query",
    "moderately relevant to the query",
    "strongly relevant to the query",
    "directly and completely answers the query",
];

const TYPESAFE_DECISION_ONLY: &str = "TypeSafe Jev is a typed decision endpoint, not a chat model; it serves only \
     graph_entity_type and query_rerank";

const PROVIDERS: &[ProviderSeed] = &[
    ProviderSeed {
        kind: "openai",
        display_name: "OpenAI",
        api_style: "openai_compatible",
        default_base_url: Some("https://api.openai.com/v1"),
        profile: OPENAI_PROFILE,
        bootstrap_presets: OPENAI_BOOTSTRAP_PRESETS,
        models: OPENAI_MODELS,
        purpose_gaps: &[],
        lifecycle_state: "active",
    },
    ProviderSeed {
        kind: "anthropic",
        display_name: "Anthropic",
        api_style: "anthropic_messages",
        default_base_url: Some("https://api.anthropic.com/v1"),
        profile: ProfileSeed::Native(NativeProfileSeed {
            runtime_kind: "anthropic_messages",
            auth_scheme: "anthropic_api_key",
            chat_path: "/messages",
            models_path: "/models",
        }),
        bootstrap_presets: ANTHROPIC_BOOTSTRAP_PRESETS,
        models: ANTHROPIC_MODELS,
        purpose_gaps: &[
            PurposeGap {
                kind: PurposeGapKind::ProviderCannot,
                purpose: BindingPurpose::EmbedChunk,
                code: "no_embeddings_endpoint",
                reason: "vendor exposes no embeddings endpoint",
            },
            PurposeGap {
                kind: PurposeGapKind::ProviderCannot,
                purpose: BindingPurpose::ExtractGraph,
                code: "no_structured_output",
                reason: NATIVE_RUNTIME_HAS_NO_STRUCTURED_OUTPUT,
            },
        ],
        lifecycle_state: "active",
    },
    ProviderSeed {
        kind: "gemini",
        display_name: "Google Gemini",
        api_style: "gemini_generate_content",
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        profile: ProfileSeed::Native(NativeProfileSeed {
            runtime_kind: "gemini_generate_content",
            auth_scheme: "google_api_key",
            chat_path: "/models/{model}:generateContent",
            models_path: "/models",
        }),
        bootstrap_presets: GEMINI_BOOTSTRAP_PRESETS,
        models: GEMINI_MODELS,
        purpose_gaps: &[PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::ExtractGraph,
            code: "no_structured_output",
            reason: NATIVE_RUNTIME_HAS_NO_STRUCTURED_OUTPUT,
        }],
        lifecycle_state: "active",
    },
    ProviderSeed {
        kind: "minimax",
        display_name: "MiniMax",
        api_style: "openai_compatible",
        default_base_url: Some("https://api.minimax.io/v1"),
        profile: COMPATIBLE_PROFILE,
        bootstrap_presets: MINIMAX_BOOTSTRAP_PRESETS,
        models: MINIMAX_MODELS,
        purpose_gaps: &[PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::EmbedChunk,
            code: "incompatible_embeddings_wire",
            reason: "embo-01 uses a non-OpenAI request shape (texts/type) that \
                     the OpenAI-compatible gateway does not speak",
        }],
        lifecycle_state: "active",
    },
    ProviderSeed {
        kind: "deepseek",
        display_name: "DeepSeek",
        api_style: "openai_compatible",
        default_base_url: Some("https://api.deepseek.com"),
        profile: DEEPSEEK_PROFILE,
        bootstrap_presets: DEEPSEEK_BOOTSTRAP_PRESETS,
        models: DEEPSEEK_MODELS,
        purpose_gaps: &[PurposeGap {
            kind: PurposeGapKind::ProviderCannot,
            purpose: BindingPurpose::EmbedChunk,
            code: "no_embeddings_endpoint",
            reason: "vendor exposes no embeddings endpoint",
        }],
        lifecycle_state: "active",
    },
    ProviderSeed::hosted_openai_compatible(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        OPENROUTER_MODELS,
        // Documented in US dollars, and the listing's numbers agree with the
        // published per-token rates.
        Some("USD"),
        &[],
    ),
    ProviderSeed::hosted_openai_compatible(
        "routerai",
        "RouterAI",
        "https://routerai.ru/api/v1",
        ROUTERAI_MODELS,
        // Roubles. The same model that OpenRouter lists at 2 per million
        // appears here at roughly 219, which is the rouble rate, not a
        // hundredfold markup. Recording it as dollars would multiply every
        // reported cost. The listing also prices the OpenAI embedding ids, so
        // RouterAI serves every purpose and declares no gap.
        Some("RUB"),
        &[],
    ),
    // GPTunneL's listing carries ids and nothing else — no rates to attribute,
    // so it stays model-less and declares its chat purposes operator-supplied.
    ProviderSeed::hosted_openai_compatible(
        "gptunnel",
        "GPTunneL",
        "https://gptunnel.ru/v1",
        &[],
        None,
        ROUTER_OPERATOR_SUPPLIED,
    ),
    ProviderSeed {
        kind: "ollama",
        display_name: "Ollama",
        api_style: "openai_compatible",
        default_base_url: None,
        profile: OLLAMA_PROFILE,
        bootstrap_presets: &[],
        models: OLLAMA_MODELS,
        purpose_gaps: OLLAMA_OPERATOR_SUPPLIED,
        lifecycle_state: "active",
    },
    ProviderSeed::hosted_openai_compatible(
        "qwen",
        "Qwen",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        QWEN_MODELS,
        Some("USD"),
        QWEN_PURPOSE_GAPS,
    ),
    ProviderSeed {
        kind: "litellm",
        display_name: "LiteLLM",
        api_style: "openai_compatible",
        default_base_url: None,
        profile: LITELLM_PROFILE,
        bootstrap_presets: &[],
        models: &[],
        purpose_gaps: LITELLM_OPERATOR_SUPPLIED,
        lifecycle_state: "active",
    },
    TYPESAFE_PROVIDER,
];

pub(crate) async fn seed_clean_v1_catalog(postgres: &PgPool) -> anyhow::Result<()> {
    let mut transaction = postgres.begin().await?;
    for provider in PROVIDERS {
        upsert_provider(&mut transaction, provider).await?;
    }
    for row in model_rows() {
        upsert_model(&mut transaction, row).await?;
    }
    // The upsert only reaches names the manifest still carries. Retire the
    // rest of this seed's own rows so an upgraded instance stops offering a
    // model whose price and id the vendor has moved on from.
    retire_seed_owned_rows_absent_from_manifest(&mut transaction).await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
fn provider_seed(kind: &str) -> Option<&'static ProviderSeed> {
    PROVIDERS.iter().find(|provider| provider.kind == kind)
}

fn model_rows() -> impl Iterator<Item = ModelRow> {
    PROVIDERS
        .iter()
        .flat_map(|provider| provider.models.iter().map(move |model| ModelRow { provider, model }))
}

/// Every `(provider kind, model name)` pair the manifest lists, the key a
/// retirement decision compares an existing row against.
fn manifest_model_keys() -> BTreeSet<(String, String)> {
    model_rows()
        .map(|row| (row.provider.kind.to_string(), row.model.model_name.to_string()))
        .collect()
}

/// One catalog row reduced to what a retirement decision needs. `seed_owned`
/// carries the `metadata_json` marker rather than a JSON value so the
/// selection below stays a pure function over plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogModelIdentity {
    id: Uuid,
    provider_kind: String,
    model_name: String,
    seed_owned: bool,
}

/// Ids of rows this seed owns whose model the manifest no longer lists.
///
/// The manifest is authoritative for its own rows only: a model the operator
/// added through the API, or one runtime discovery created, must survive an
/// upgrade even though the seed has never heard of it. Pure so the selection
/// is tested without a database.
fn seed_owned_rows_absent_from_manifest(
    existing: &[CatalogModelIdentity],
    manifest: &BTreeSet<(String, String)>,
) -> Vec<Uuid> {
    existing
        .iter()
        .filter(|row| row.seed_owned)
        .filter(|row| !manifest.contains(&(row.provider_kind.clone(), row.model_name.clone())))
        .map(|row| row.id)
        .collect()
}

/// Mark as `deprecated` every row this seed wrote in an earlier release that
/// the manifest no longer lists. `deprecated` is `ai_model_lifecycle_state`'s
/// terminal value for a model kept for continuity; the row is not deleted
/// because an existing binding may still reference it, and not `disabled`,
/// which would read as an operator's own decision.
async fn retire_seed_owned_rows_absent_from_manifest(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let existing = sqlx::query_as::<_, (Uuid, String, String, serde_json::Value)>(
        "select model.id, provider.provider_kind, model.model_name, model.metadata_json
         from ai_model_catalog model
         join ai_provider_catalog provider on provider.id = model.provider_catalog_id",
    )
    .fetch_all(&mut **transaction)
    .await?;
    let rows = existing
        .into_iter()
        .map(|(id, provider_kind, model_name, metadata_json)| CatalogModelIdentity {
            id,
            provider_kind,
            model_name,
            seed_owned: metadata_json
                .get(SEED_SOURCE_METADATA_KEY)
                .and_then(|value| value.as_str())
                == Some(SEED_SOURCE_CLEAN_V1),
        })
        .collect::<Vec<_>>();
    let stale = seed_owned_rows_absent_from_manifest(&rows, &manifest_model_keys());
    if stale.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "update ai_model_catalog
         set lifecycle_state = 'deprecated'::ai_model_lifecycle_state
         where id = any($1::uuid[])
           and metadata_json ->> $2 = $3",
    )
    .bind(&stale)
    .bind(SEED_SOURCE_METADATA_KEY)
    .bind(SEED_SOURCE_CLEAN_V1)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn seed_uuid(parts: &[&str]) -> Uuid {
    Uuid::new_v5(&SEED_NAMESPACE, parts.join(":").as_bytes())
}

async fn upsert_provider(
    transaction: &mut Transaction<'_, Postgres>,
    provider: &ProviderSeed,
) -> anyhow::Result<()> {
    let profile = provider_profile(provider);
    sqlx::query("insert into ai_provider_catalog (id, provider_kind, display_name, api_style, lifecycle_state, default_base_url, capability_flags_json) values ($1, $2, $3, $4::ai_provider_api_style, $7::ai_provider_lifecycle_state, $5, $6) on conflict (provider_kind) do update set display_name = excluded.display_name, api_style = excluded.api_style, lifecycle_state = excluded.lifecycle_state, default_base_url = excluded.default_base_url, capability_flags_json = excluded.capability_flags_json where (ai_provider_catalog.display_name, ai_provider_catalog.api_style, ai_provider_catalog.lifecycle_state, ai_provider_catalog.default_base_url, ai_provider_catalog.capability_flags_json) is distinct from (excluded.display_name, excluded.api_style, excluded.lifecycle_state, excluded.default_base_url, excluded.capability_flags_json)")
        .bind(seed_uuid(&["ironrag", provider.kind, "provider"])).bind(provider.kind).bind(provider.display_name).bind(provider.api_style).bind(provider.default_base_url).bind(profile).bind(provider.lifecycle_state).execute(&mut **transaction).await?;
    Ok(())
}

async fn upsert_model(
    transaction: &mut Transaction<'_, Postgres>,
    row: ModelRow,
) -> anyhow::Result<()> {
    let provider_id = seed_uuid(&["ironrag", row.provider.kind, "provider"]);
    let seeded_model_id = seed_uuid(&["ironrag", row.provider.kind, "model", row.model.model_name]);
    let mut metadata = serde_json::json!({"observedInputModalities": row.model.observed_input_modalities, "seedSource": SEED_SOURCE_CLEAN_V1});
    if let Some(dimensions) = row.model.embedding_dimensions {
        metadata["dimensions"] = serde_json::json!(dimensions);
    }
    let pricing = row.model.pricing;
    let long_context = pricing.long_context;
    sqlx::query("insert into ai_model_catalog (id, provider_catalog_id, model_name, context_window, max_output_tokens, lifecycle_state, metadata_json, currency_code, input_price_per_1m, cached_input_price_per_1m, cache_write_input_price_per_1m, output_price_per_1m, long_context_threshold_tokens, long_context_input_price_per_1m, long_context_cached_input_price_per_1m, long_context_output_price_per_1m) values ($1, $2, $3, $4, $5, $16::ai_model_lifecycle_state, $6, $7, $8::numeric, $9::numeric, $10::numeric, $11::numeric, $12, $13::numeric, $14::numeric, $15::numeric) on conflict (provider_catalog_id, model_name) do update set context_window = excluded.context_window, max_output_tokens = excluded.max_output_tokens, lifecycle_state = excluded.lifecycle_state, metadata_json = excluded.metadata_json, currency_code = excluded.currency_code, input_price_per_1m = excluded.input_price_per_1m, cached_input_price_per_1m = excluded.cached_input_price_per_1m, cache_write_input_price_per_1m = excluded.cache_write_input_price_per_1m, output_price_per_1m = excluded.output_price_per_1m, long_context_threshold_tokens = excluded.long_context_threshold_tokens, long_context_input_price_per_1m = excluded.long_context_input_price_per_1m, long_context_cached_input_price_per_1m = excluded.long_context_cached_input_price_per_1m, long_context_output_price_per_1m = excluded.long_context_output_price_per_1m where (ai_model_catalog.context_window, ai_model_catalog.max_output_tokens, ai_model_catalog.lifecycle_state, ai_model_catalog.metadata_json, ai_model_catalog.currency_code, ai_model_catalog.input_price_per_1m, ai_model_catalog.cached_input_price_per_1m, ai_model_catalog.cache_write_input_price_per_1m, ai_model_catalog.output_price_per_1m, ai_model_catalog.long_context_threshold_tokens, ai_model_catalog.long_context_input_price_per_1m, ai_model_catalog.long_context_cached_input_price_per_1m, ai_model_catalog.long_context_output_price_per_1m) is distinct from (excluded.context_window, excluded.max_output_tokens, excluded.lifecycle_state, excluded.metadata_json, excluded.currency_code, excluded.input_price_per_1m, excluded.cached_input_price_per_1m, excluded.cache_write_input_price_per_1m, excluded.output_price_per_1m, excluded.long_context_threshold_tokens, excluded.long_context_input_price_per_1m, excluded.long_context_cached_input_price_per_1m, excluded.long_context_output_price_per_1m)")
        .bind(seeded_model_id).bind(provider_id).bind(row.model.model_name).bind(row.model.context_window).bind(row.model.max_output_tokens).bind(metadata)
        .bind(row.provider.listed_pricing_currency())
        .bind(pricing.input).bind(pricing.cached_input).bind(pricing.cache_write_input).bind(pricing.output)
        .bind(long_context.map(|tier| tier.threshold_tokens))
        .bind(long_context.map(|tier| tier.input))
        .bind(long_context.map(|tier| tier.cached_input))
        .bind(long_context.map(|tier| tier.output))
        .bind(row.model.lifecycle_state)
        .execute(&mut **transaction).await?;
    // `on conflict do update ... where` skips the write when the row already
    // matches, so `returning id` would return no row on the no-op path. Read the
    // identity back instead; the unique `(provider_catalog_id, model_name)`
    // constraint makes it exactly one row.
    let model_id = sqlx::query_scalar::<_, Uuid>(
        "select id from ai_model_catalog where provider_catalog_id = $1 and model_name = $2",
    )
    .bind(provider_id)
    .bind(row.model.model_name)
    .fetch_one(&mut **transaction)
    .await?;
    let binding_purposes = row
        .model
        .binding_purposes
        .iter()
        .map(|purpose| purpose.as_str().to_string())
        .collect::<Vec<_>>();
    sqlx::query("with desired_purposes as (select distinct purpose::ai_binding_purpose as binding_purpose from unnest($2::text[]) as purpose), inserted_purposes as (insert into ai_model_binding_purpose (model_catalog_id, binding_purpose) select $1, desired_purposes.binding_purpose from desired_purposes on conflict (model_catalog_id, binding_purpose) do nothing) delete from ai_model_binding_purpose relation where relation.model_catalog_id = $1 and not exists (select 1 from desired_purposes where desired_purposes.binding_purpose = relation.binding_purpose)")
        .bind(model_id).bind(binding_purposes).execute(&mut **transaction).await?;
    Ok(())
}

fn provider_profile(provider: &ProviderSeed) -> serde_json::Value {
    let mut profile = match provider.profile {
        ProfileSeed::Native(profile) => native_profile(profile),
        ProfileSeed::OpenAiCompatible(profile) => openai_profile(profile),
        ProfileSeed::Decision(profile) => decision_profile(profile),
    };
    profile["bootstrapPresets"] = serde_json::Value::Array(
        provider.bootstrap_presets.iter().map(bootstrap_preset_json).collect(),
    );
    // Carried into the provider row so setup can say "this provider cannot
    // embed, add a second one" while the operator is still on the setup
    // screen. Discovering it at ingest time, as BindingNotConfigured, is what
    // this field exists to prevent. The kind travels with it because the two
    // gaps need opposite words: one is a dead end, the other is a prompt to
    // pick a model.
    profile["purposeGaps"] = serde_json::Value::Array(
        provider
            .purpose_gaps
            .iter()
            .map(|gap| serde_json::json!({"purpose": gap.purpose.as_str(), "kind": gap.kind.as_str(), "code": gap.code, "reason": gap.reason}))
            .collect(),
    );
    profile
}

fn bootstrap_preset_json(preset: &BootstrapPreset) -> serde_json::Value {
    let mut entry =
        serde_json::json!({"purpose": preset.purpose.as_str(), "modelName": preset.model_name});
    if !preset.parameters.is_empty() {
        entry["extraParametersJson"] = serde_json::Value::Object(
            preset
                .parameters
                .iter()
                .map(|(key, value)| ((*key).to_string(), serde_json::json!(value)))
                .collect(),
        );
    }
    entry
}

/// Test-support view of one seeded provider, so a live suite builds its binding
/// from the canonical seed instead of a hand-copied profile.
#[cfg(test)]
pub(crate) struct SeededProviderView {
    pub(crate) api_style: &'static str,
    pub(crate) default_base_url: Option<&'static str>,
    /// The `_providerProfile` exactly as the catalog stores it.
    pub(crate) profile: serde_json::Value,
}

#[cfg(test)]
pub(crate) fn seeded_provider(kind: &str) -> Option<SeededProviderView> {
    PROVIDERS.iter().find(|provider| provider.kind == kind).map(|provider| SeededProviderView {
        api_style: provider.api_style,
        default_base_url: provider.default_base_url,
        profile: provider_profile(provider),
    })
}

/// Seeded vendor rate of one model as `(input, output)` per one million tokens
/// in the provider's listed currency; `None` when the model is not seeded.
#[cfg(test)]
pub(crate) fn seeded_model_rate(
    provider_kind: &str,
    model_name: &str,
) -> Option<(&'static str, &'static str)> {
    PROVIDERS
        .iter()
        .find(|provider| provider.kind == provider_kind)?
        .models
        .iter()
        .find(|model| model.model_name == model_name)
        .map(|model| (model.pricing.input, model.pricing.output))
}

fn native_profile(profile: NativeProfileSeed) -> serde_json::Value {
    serde_json::json!({"runtime": {"kind": profile.runtime_kind, "authScheme": profile.auth_scheme, "tokenLimitParameter": "max_tokens", "structuredOutput": "unsupported", "chatPath": profile.chat_path, "embeddingsPath": null, "modelsPath": profile.models_path}, "credentials": {"apiKeyRequired": true, "baseUrlRequired": false, "baseUrlMode": "fixed", "validationMode": "model_list"}, "baseUrl": {"allowOverride": false, "requireHttps": true, "allowPrivateNetwork": false, "trimSuffixes": []}, "modelDiscovery": {"mode": "credential", "paths": [{"capabilityKind": "chat", "path": profile.models_path}], "listedPricingCurrency": null}, "capabilities": {"chat": "supported", "embeddings": "unsupported", "vision": "supported", "streaming": "unsupported", "tools": "unsupported", "modelDiscovery": "supported"}, "uiHints": {"credentialFields": ["api_key"], "baseUrlEditable": false}, "serviceTierMultipliers": {}})
}

/// A typed decision provider's profile (IRG-182): every generative
/// capability is `unsupported`, `decision` is the only `supported` one, and
/// the sole discovery path declares `capabilityKind: decision` — the shape
/// `validate_decision_capability_invariant` requires of every provider whose
/// `runtime.kind` is `typesafe_system_one`.
fn decision_profile(profile: DecisionProfileSeed) -> serde_json::Value {
    serde_json::json!({"runtime": {"kind": profile.runtime_kind, "authScheme": "bearer", "tokenLimitParameter": "max_tokens", "structuredOutput": "unsupported", "chatPath": profile.chat_path, "embeddingsPath": null, "modelsPath": profile.models_path, "scoreCriteria": profile.score_criteria}, "credentials": {"apiKeyRequired": true, "baseUrlRequired": false, "baseUrlMode": "fixed", "validationMode": "model_list"}, "baseUrl": {"allowOverride": false, "requireHttps": true, "allowPrivateNetwork": false, "trimSuffixes": []}, "modelDiscovery": {"mode": "credential", "paths": [{"capabilityKind": "decision", "path": profile.models_path}], "listedPricingCurrency": null}, "capabilities": {"chat": "unsupported", "embeddings": "unsupported", "vision": "unsupported", "streaming": "unsupported", "tools": "unsupported", "modelDiscovery": "supported", "decision": "supported"}, "uiHints": {"credentialFields": ["api_key"], "baseUrlEditable": false}, "serviceTierMultipliers": {}})
}

fn openai_profile(profile: OpenAiProfileSeed) -> serde_json::Value {
    let model_paths = if profile.model_discovery {
        let mut paths = vec![serde_json::json!({"capabilityKind": "chat", "path": "/models"})];
        if profile.embedding_discovery {
            paths.push(serde_json::json!({"capabilityKind": "embedding", "path": "/models"}));
        }
        serde_json::Value::Array(paths)
    } else {
        serde_json::json!([])
    };
    let service_tier_multipliers = profile
        .service_tier_multipliers
        .iter()
        .map(|(tier, multiplier)| {
            ((*tier).to_string(), serde_json::Value::String((*multiplier).to_string()))
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();
    serde_json::json!({"runtime": {"kind": "openai_compatible", "authScheme": "bearer", "tokenLimitParameter": profile.token_limit_parameter, "structuredOutput": profile.structured_output, "chatPath": "/chat/completions", "embeddingsPath": "/embeddings", "modelsPath": profile.model_discovery.then_some("/models")}, "credentials": {"apiKeyRequired": profile.api_key_required, "baseUrlRequired": !profile.base_url_mode.is_fixed(), "baseUrlMode": profile.base_url_mode.as_str(), "validationMode": if profile.validate_credentials_on_save { "model_list" } else { "none" }}, "baseUrl": {"allowOverride": !profile.base_url_mode.is_fixed(), "requireHttps": !profile.allow_private_network, "allowPrivateNetwork": profile.allow_private_network, "trimSuffixes": ["/v1/chat/completions", "/chat/completions", "/v1/models", "/models"]}, "modelDiscovery": {"mode": if profile.model_discovery { "credential" } else { "unsupported" }, "paths": model_paths, "listedPricingCurrency": profile.listed_pricing_currency}, "capabilities": {"chat": "supported", "embeddings": if profile.embedding_discovery { "supported" } else { "unknown" }, "vision": if profile.native_capabilities { "supported" } else { "unknown" }, "streaming": "unknown", "tools": if profile.native_capabilities { "supported" } else { "unknown" }, "modelDiscovery": if profile.model_discovery { "supported" } else { "unsupported" }, "flexTier": if profile.flex_tier { "supported" } else { "unsupported" }}, "uiHints": {"credentialFields": if profile.api_key_required { serde_json::json!(["api_key"]) } else { serde_json::json!([]) }, "baseUrlEditable": !profile.base_url_mode.is_fixed()}, "requestPolicy": {"sampling": profile.sampling, "reasoning": profile.reasoning, "toolChoice": profile.tool_choice, "syntheticToolHistory": profile.synthetic_tool_history}, "serviceTierMultipliers": service_tier_multipliers})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_catalog_constructors_preserve_explicit_manifest_fields_at_runtime() {
        assert_native_chat_constructor_runtime_fields();
        assert_embedding_constructor_runtime_fields();
        assert_pricing_with_cached_constructor_fields();
        assert_hosted_provider_constructor_runtime_fields();
    }

    /// `ModelSeed::native_multimodal_chat` must carry the modalities, windows
    /// and binding purposes its literal arguments name, not the compact
    /// constructor's own idea of them.
    fn assert_native_chat_constructor_runtime_fields() {
        let model = ModelSeed::native_multimodal_chat(
            std::hint::black_box("model-runtime"),
            std::hint::black_box(200_000),
            std::hint::black_box(64_000),
            std::hint::black_box(FABLE_PRICES),
        );
        assert_eq!(model.observed_input_modalities, TEXT_AND_IMAGE);
        assert_eq!(model.context_window, Some(200_000));
        assert_eq!(model.max_output_tokens, Some(64_000));
        assert_eq!(
            model.binding_purposes.iter().map(|purpose| purpose.as_str()).collect::<Vec<_>>(),
            [
                "extract_text",
                "query_compile",
                "query_rerank",
                "graph_entity_type",
                "query_answer",
                "agent",
            ]
        );
    }

    /// `ModelSeed::embedding` is text-only and carries the caller's width and
    /// rate, with a zero output price.
    fn assert_embedding_constructor_runtime_fields() {
        let embedding = ModelSeed::embedding(
            std::hint::black_box("embedding-runtime"),
            std::hint::black_box(3_072),
            std::hint::black_box(&[BindingPurpose::EmbedChunk]),
            std::hint::black_box("0.13"),
        );
        assert_eq!(embedding.embedding_dimensions, Some(3_072));
        assert_eq!(embedding.observed_input_modalities, TEXT);
        assert_eq!(embedding.pricing.input, "0.13");
        assert_eq!(embedding.pricing.output, "0");
    }

    /// `PricingSeed::with_cached` keeps the cached rate and leaves the
    /// cache-write rate at its zero default.
    fn assert_pricing_with_cached_constructor_fields() {
        let cached = PricingSeed::with_cached(
            std::hint::black_box("0.2"),
            std::hint::black_box("0.02"),
            std::hint::black_box("1.25"),
        );
        assert_eq!(cached.cached_input, "0.02");
        assert_eq!(cached.cache_write_input, "0");
    }

    /// `ProviderSeed::hosted_openai_compatible` publishes the caller's base
    /// URL, profile kind and model set, with no presets of its own.
    fn assert_hosted_provider_constructor_runtime_fields() {
        let provider = ProviderSeed::hosted_openai_compatible(
            std::hint::black_box("provider-runtime"),
            std::hint::black_box("Provider Runtime"),
            std::hint::black_box("https://example.invalid/v1"),
            std::hint::black_box(OPENAI_MODELS),
            std::hint::black_box(Some("USD")),
            std::hint::black_box(ROUTER_OPERATOR_SUPPLIED),
        );
        assert_eq!(provider.api_style, "openai_compatible");
        assert_eq!(provider.default_base_url, Some("https://example.invalid/v1"));
        assert!(matches!(provider.profile, ProfileSeed::OpenAiCompatible(_)));
        assert!(provider.bootstrap_presets.is_empty());
        assert_eq!(provider.models.len(), OPENAI_MODELS.len());
    }

    #[test]
    fn provider_profiles_declare_auto_tool_choice_only_for_deepseek() {
        let auto_only = PROVIDERS
            .iter()
            .filter(|provider| {
                provider_profile(provider)["requestPolicy"]["toolChoice"].as_str()
                    == Some("auto_only")
            })
            .map(|provider| provider.kind)
            .collect::<Vec<_>>();

        assert_eq!(auto_only, ["deepseek"]);
    }

    #[test]
    fn provider_profiles_declare_inline_context_only_for_deepseek() {
        let inline = PROVIDERS
            .iter()
            .filter(|provider| {
                provider_profile(provider)["requestPolicy"]["syntheticToolHistory"].as_str()
                    == Some("inline")
            })
            .map(|provider| provider.kind)
            .collect::<Vec<_>>();

        assert_eq!(inline, ["deepseek"]);
    }

    #[test]
    fn provider_profiles_keep_sampling_policies_provider_specific() {
        let sampling = PROVIDERS
            .iter()
            .filter(|provider| {
                matches!(
                    provider.kind,
                    "openai"
                        | "minimax"
                        | "deepseek"
                        | "openrouter"
                        | "routerai"
                        | "gptunnel"
                        | "ollama"
                        | "qwen"
                        | "litellm"
                )
            })
            .map(|provider| {
                (
                    provider.kind,
                    provider_profile(provider)["requestPolicy"]["sampling"]
                        .as_str()
                        .expect("seeded sampling policy must be a string")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            sampling,
            [
                ("openai", "omit".to_string()),
                ("minimax", "forward".to_string()),
                ("deepseek", "omit".to_string()),
                ("openrouter", "forward".to_string()),
                ("routerai", "forward".to_string()),
                ("gptunnel", "forward".to_string()),
                ("ollama", "forward".to_string()),
                ("qwen", "forward".to_string()),
                ("litellm", "forward".to_string()),
            ]
        );
    }

    #[test]
    fn provider_profiles_declare_reasoning_control_only_where_verified() {
        let disabling = PROVIDERS
            .iter()
            .filter(|provider| {
                provider_profile(provider)["requestPolicy"]["reasoning"].as_str()
                    == Some("disable_with_none")
            })
            .map(|provider| provider.kind)
            .collect::<Vec<_>>();

        assert_eq!(
            disabling,
            ["openai", "deepseek"],
            "only the provider whose endpoint verified the reasoning_effort control may disable hidden reasoning"
        );

        let openai = provider_seed("openai").expect("OpenAI provider seed must exist");
        assert_eq!(
            provider_profile(openai)["requestPolicy"]["reasoning"].as_str(),
            Some("disable_with_none")
        );
    }

    #[test]
    fn provider_profiles_keep_structured_output_capabilities_provider_specific() {
        let modes = PROVIDERS
            .iter()
            .filter(|provider| {
                matches!(
                    provider.kind,
                    "openai"
                        | "minimax"
                        | "deepseek"
                        | "openrouter"
                        | "routerai"
                        | "gptunnel"
                        | "ollama"
                        | "qwen"
                        | "litellm"
                )
            })
            .map(|provider| {
                (
                    provider.kind,
                    provider_profile(provider)["runtime"]["structuredOutput"]
                        .as_str()
                        .expect("seeded structured-output mode must be a string")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            modes,
            [
                ("openai", "json_schema".to_string()),
                ("minimax", "prompt_only_json_object".to_string()),
                ("deepseek", "json_object".to_string()),
                ("openrouter", "prompt_only_json_object".to_string()),
                ("routerai", "prompt_only_json_object".to_string()),
                ("gptunnel", "prompt_only_json_object".to_string()),
                ("ollama", "prompt_only_json_object".to_string()),
                ("qwen", "prompt_only_json_object".to_string()),
                ("litellm", "prompt_only_json_object".to_string()),
            ]
        );

        for provider in PROVIDERS.iter().filter(|provider| {
            matches!(
                provider.kind,
                "minimax"
                    | "deepseek"
                    | "openrouter"
                    | "routerai"
                    | "gptunnel"
                    | "ollama"
                    | "qwen"
                    | "litellm"
            )
        }) {
            let capabilities = provider_profile(provider)["capabilities"].clone();
            assert_eq!(capabilities["embeddings"], "unknown", "{} embeddings", provider.kind);
            assert_eq!(capabilities["tools"], "unknown", "{} tools", provider.kind);
            assert_eq!(capabilities["vision"], "unknown", "{} vision", provider.kind);
        }
    }

    #[test]
    fn openai_profile_covers_native_bootstrap_capabilities_and_discovery() {
        let openai = PROVIDERS
            .iter()
            .find(|provider| provider.kind == "openai")
            .expect("OpenAI provider seed must exist");
        let profile = provider_profile(openai);
        let paths = profile["modelDiscovery"]["paths"]
            .as_array()
            .expect("OpenAI model discovery paths must be an array");

        assert_eq!(profile["capabilities"]["embeddings"], "supported");
        assert_eq!(profile["capabilities"]["tools"], "supported");
        assert_eq!(profile["capabilities"]["vision"], "supported");
        assert_eq!(
            profile["bootstrapPresets"],
            serde_json::json!([
                {"purpose": "extract_text", "modelName": "gpt-6-luna",
                 "extraParametersJson": {"service_tier": "flex"}},
                {"purpose": "extract_graph", "modelName": "gpt-6-luna",
                 "extraParametersJson": {"reasoning_effort": "none", "service_tier": "flex"}},
                {"purpose": "embed_chunk", "modelName": "text-embedding-3-large"},
                {"purpose": "query_compile", "modelName": "gpt-6-luna"},
                {"purpose": "query_rerank", "modelName": "gpt-6-luna",
                 "extraParametersJson": {"reasoning_effort": "none"}},
                {"purpose": "graph_entity_type", "modelName": "gpt-6-luna",
                 "extraParametersJson": {"reasoning_effort": "none", "service_tier": "flex"}},
                {"purpose": "query_answer", "modelName": "gpt-6-luna"},
                {"purpose": "agent", "modelName": "gpt-6-luna"},
            ])
        );
        assert_eq!(
            paths,
            &[
                serde_json::json!({"capabilityKind": "chat", "path": "/models"}),
                serde_json::json!({"capabilityKind": "embedding", "path": "/models"}),
            ]
        );
    }

    #[test]
    fn only_openai_declares_the_flex_service_tier() {
        let flex = PROVIDERS
            .iter()
            .filter(|provider| {
                provider_profile(provider)["capabilities"]["flexTier"] == "supported"
            })
            .map(|provider| provider.kind)
            .collect::<Vec<_>>();
        assert_eq!(flex, ["openai"]);
        // Every preset that asks for Flex belongs to a provider that declares it.
        for provider in PROVIDERS {
            let declares = provider_profile(provider)["capabilities"]["flexTier"] == "supported";
            for preset in provider.bootstrap_presets {
                let asks = preset.parameters.contains(&OPENAI_FLEX);
                assert!(!asks || declares, "{} preset {}", provider.kind, preset.purpose.as_str());
            }
        }
    }

    #[test]
    fn only_openai_declares_published_service_tier_multipliers() {
        let openai = provider_seed("openai").expect("OpenAI provider seed must exist");
        assert_eq!(
            provider_profile(openai)["serviceTierMultipliers"],
            serde_json::json!({"flex": "0.5", "priority": "2"})
        );
        for provider in PROVIDERS.iter().filter(|provider| provider.kind != "openai") {
            assert_eq!(
                provider_profile(provider)["serviceTierMultipliers"],
                serde_json::json!({}),
                "{} must not declare tiered pricing it does not publish",
                provider.kind
            );
        }
    }

    #[test]
    fn catalog_manifest_keeps_exact_openai_bootstrap_presets() {
        let openai = provider_seed("openai").expect("OpenAI provider seed must exist");
        let presets = openai
            .bootstrap_presets
            .iter()
            .map(|preset| (preset.purpose.as_str(), preset.model_name))
            .collect::<Vec<_>>();

        assert_eq!(
            presets,
            [
                ("extract_text", "gpt-6-luna"),
                ("extract_graph", "gpt-6-luna"),
                ("embed_chunk", "text-embedding-3-large"),
                ("query_compile", "gpt-6-luna"),
                ("query_rerank", "gpt-6-luna"),
                ("graph_entity_type", "gpt-6-luna"),
                ("query_answer", "gpt-6-luna"),
                ("agent", "gpt-6-luna"),
            ]
        );
        assert!(openai.bootstrap_presets.iter().all(|preset| {
            openai.models.iter().any(|model| {
                model.model_name == preset.model_name
                    && model.binding_purposes.contains(&preset.purpose)
            })
        }));
    }

    #[test]
    fn catalog_manifest_contains_only_supported_minimax_models() {
        let minimax = provider_seed("minimax").expect("MiniMax provider seed must exist");
        let models = minimax.models.iter().map(|model| model.model_name).collect::<Vec<_>>();
        assert_eq!(models, ["MiniMax-M3", "MiniMax-M2.7", "MiniMax-M2.7-highspeed"]);
    }

    #[test]
    /// Rates are the 2026-09-17 pricing, seeded at DeepSeek's PEAK tier —
    /// the vendor charges half off-peak (01:00-04:00 and 06:00-10:00 UTC are
    /// peak) and a single seeded rate cannot express that, so the catalog
    /// carries the one that cannot understate a bill.
    fn catalog_manifest_contains_current_deepseek_v4_models_and_vendor_pricing() {
        let deepseek = provider_seed("deepseek").expect("DeepSeek provider seed must exist");
        assert_eq!(deepseek.display_name, "DeepSeek");
        assert_eq!(deepseek.api_style, "openai_compatible");
        assert_eq!(deepseek.default_base_url, Some("https://api.deepseek.com"));
        assert_eq!(
            deepseek
                .models
                .iter()
                .map(|model| {
                    (
                        model.model_name,
                        model.observed_input_modalities,
                        model.context_window,
                        model.max_output_tokens,
                        model
                            .binding_purposes
                            .iter()
                            .map(|purpose| purpose.as_str())
                            .collect::<Vec<_>>(),
                        model.pricing.input,
                        model.pricing.cached_input,
                        model.pricing.output,
                    )
                })
                .collect::<Vec<_>>(),
            [
                (
                    "deepseek-v4-pro",
                    TEXT,
                    Some(1_000_000),
                    Some(384_000),
                    vec![
                        "extract_graph",
                        "query_compile",
                        "query_rerank",
                        "graph_entity_type",
                        "query_answer",
                        "agent",
                    ],
                    "1.32",
                    "0.044",
                    "3.96",
                ),
                (
                    "deepseek-flash",
                    TEXT_AND_IMAGE,
                    Some(1_000_000),
                    Some(384_000),
                    vec![
                        "extract_text",
                        "extract_graph",
                        "query_compile",
                        "query_rerank",
                        "graph_entity_type",
                        "query_answer",
                        "agent",
                    ],
                    "0.3",
                    "0.006",
                    "1.2",
                ),
            ]
        );

        let profile = provider_profile(deepseek);
        assert_eq!(profile["runtime"]["kind"], "openai_compatible");
        assert_eq!(profile["runtime"]["structuredOutput"], "json_object");
        assert_eq!(profile["requestPolicy"]["sampling"], "omit");
        assert_eq!(profile["credentials"]["validationMode"], "model_list");
        assert_eq!(profile["modelDiscovery"]["mode"], "credential");
        assert_eq!(
            profile["modelDiscovery"]["paths"],
            serde_json::json!([{"capabilityKind": "chat", "path": "/models"}])
        );
    }

    #[test]
    fn openai_compatible_profiles_emit_provider_specific_token_limit_parameters() {
        let parameter = |kind| {
            let provider = provider_seed(kind).expect("provider seed must exist");
            provider_profile(provider)["runtime"]["tokenLimitParameter"]
                .as_str()
                .expect("seeded token-limit parameter must be a string")
                .to_string()
        };

        assert_eq!(
            [("openai", parameter("openai")), ("deepseek", parameter("deepseek"))],
            [
                ("openai", "max_completion_tokens".to_string()),
                ("deepseek", "max_tokens".to_string()),
            ]
        );
    }

    #[test]
    fn every_seeded_model_carries_explicit_pricing() {
        for row in model_rows() {
            assert_row_prices_are_non_negative(row);
        }

        assert_listed_pricing_currency_is_declared();
        assert_bootstrap_models_carry_real_rates();
        assert_luna_carries_its_long_context_tier();
    }

    /// Every price a seeded row carries parses as a non-negative number, and a
    /// long-context tier bills above a positive token threshold.
    fn assert_row_prices_are_non_negative(row: ModelRow) {
        let pricing = row.model.pricing;
        for value in
            [pricing.input, pricing.cached_input, pricing.cache_write_input, pricing.output]
        {
            assert!(
                value.parse::<f64>().is_ok_and(|price| price >= 0.0),
                "{}/{} carries a non-numeric price",
                row.provider.kind,
                row.model.model_name
            );
        }
        if let Some(tier) = pricing.long_context {
            assert!(tier.threshold_tokens > 0);
            for value in [tier.input, tier.cached_input, tier.output] {
                assert!(value.parse::<f64>().is_ok_and(|price| price >= 0.0));
            }
        }
    }

    /// Currency is half of a price: a rouble router row billed as dollars
    /// multiplies every reported cost, so each provider declares the unit its
    /// listing quotes and the row carries it.
    fn assert_listed_pricing_currency_is_declared() {
        assert_eq!(listed_pricing_currency_for("openai"), "USD");
        assert_eq!(listed_pricing_currency_for("openrouter"), "USD");
        assert_eq!(listed_pricing_currency_for("routerai"), "RUB");
        assert_eq!(
            model_rows().filter(|row| row.provider.kind == "routerai").count(),
            20,
            "RouterAI must seed a priced chat set and its two embedding ids, and those rows carry RUB"
        );
    }

    fn listed_pricing_currency_for(kind: &str) -> String {
        provider_profile(provider_seed(kind).expect("provider seed must exist"))["modelDiscovery"]
            ["listedPricingCurrency"]
            .as_str()
            .unwrap_or("USD")
            .to_string()
    }

    /// The zero-cost incident started with bootstrap models seeded without
    /// prices, so the two the presets bind must carry a real vendor rate.
    fn assert_bootstrap_models_carry_real_rates() {
        let bootstrap_models = ["gpt-6-luna", "text-embedding-3-large"];
        for name in bootstrap_models {
            let model = model_rows()
                .find(|row| row.provider.kind == "openai" && row.model.model_name == name)
                .expect("bootstrap model must be seeded");
            assert!(
                model.model.pricing.input.parse::<f64>().unwrap() > 0.0,
                "{name} must carry a real vendor rate: the zero-cost incident \
                 started with bootstrap models seeded without prices"
            );
        }
    }

    fn assert_luna_carries_its_long_context_tier() {
        let luna = model_rows()
            .find(|row| row.model.model_name == "gpt-5.6-luna")
            .expect("luna seed")
            .model
            .pricing;
        let tier = luna.long_context.expect("luna has a long-context tier");
        assert_eq!(tier.threshold_tokens, 272_000);
        assert_eq!((luna.input, luna.output), ("0.2", "1.2"));
        assert_eq!((tier.input, tier.output), ("0.4", "1.8"));
    }

    #[test]
    fn native_profiles_use_native_runtime_kinds() {
        let anthropic = provider_seed("anthropic").expect("Anthropic provider seed must exist");
        let gemini = provider_seed("gemini").expect("Gemini provider seed must exist");
        assert_eq!(provider_profile(anthropic)["runtime"]["kind"], "anthropic_messages");
        assert_eq!(provider_profile(gemini)["runtime"]["kind"], "gemini_generate_content");
    }

    #[test]
    fn model_purposes_are_explicit_and_native_models_exclude_graph_extraction() {
        assert!(model_rows().all(|row| !row.model.binding_purposes.is_empty()));
        assert!(
            model_rows()
                .filter(|row| matches!(row.provider.kind, "anthropic" | "gemini"))
                .all(|row| !row.model.binding_purposes.contains(&BindingPurpose::ExtractGraph))
        );
        assert!(
            provider_seed("minimax")
                .expect("MiniMax provider seed must exist")
                .models
                .iter()
                .any(|model| model.binding_purposes.contains(&BindingPurpose::ExtractGraph))
        );
    }

    #[test]
    fn query_rerank_is_eligible_wherever_query_compile_is() {
        // Semantic rerank resolves its own binding, so a model the catalog
        // offers for the compiler must also be offered for rerank: otherwise
        // enabling rerank strands every model the catalog already advertised
        // for query understanding, and the operator discovers the gap only at
        // request time. The catalog may add rerank to fewer purposes later, but
        // it must never be behind compile.
        for row in model_rows() {
            if row.model.binding_purposes.contains(&BindingPurpose::QueryCompile) {
                assert!(
                    row.model.binding_purposes.contains(&BindingPurpose::QueryRerank),
                    "{}/{} serves query_compile without query_rerank",
                    row.provider.kind,
                    row.model.model_name
                );
            }
        }
    }

    /// The purposes a provider can serve at all, given the models it seeds.
    fn seeded_purposes(provider: &ProviderSeed) -> std::collections::BTreeSet<&'static str> {
        provider
            .models
            .iter()
            .flat_map(|model| model.binding_purposes.iter().map(|purpose| purpose.as_str()))
            .collect()
    }

    #[test]
    fn every_binding_purpose_has_at_least_one_seeded_model() {
        // A purpose with no model anywhere in the catalog is not a gap a user
        // can work around: they find out at ingest time, as
        // `BindingNotConfigured: embed_chunk`, after uploading a document.
        let seeded: std::collections::BTreeSet<&str> =
            PROVIDERS.iter().flat_map(|provider| seeded_purposes(provider)).collect();
        for purpose in [
            "extract_text",
            "extract_graph",
            "embed_chunk",
            "query_compile",
            "query_answer",
            "agent",
        ] {
            assert!(seeded.contains(purpose), "no seeded model can serve {purpose}");
        }
    }

    #[test]
    fn embeddings_are_reachable_without_an_openai_account() {
        // The catalog shipped a single embedding model, on OpenAI, while every
        // other purpose had many. A deployment holding only an Anthropic,
        // Gemini or router key could configure five purposes out of six and
        // then fail to ingest a single document. At least one non-OpenAI
        // provider must be able to embed.
        let embedding_providers = PROVIDERS
            .iter()
            .filter(|provider| seeded_purposes(provider).contains("embed_chunk"))
            .map(|provider| provider.kind)
            .collect::<Vec<_>>();
        assert!(
            embedding_providers.iter().any(|kind| *kind != "openai"),
            "only OpenAI can embed: {embedding_providers:?}"
        );
    }

    #[test]
    fn bootstrap_presets_cover_every_purpose_their_provider_can_serve() {
        // A preset set that skips a purpose the provider *can* serve leaves the
        // user to discover the gap by hand. Either cover it or do not ship
        // presets for that provider at all.
        for provider in PROVIDERS.iter().filter(|provider| !provider.bootstrap_presets.is_empty()) {
            let preset_purposes = provider
                .bootstrap_presets
                .iter()
                .map(|preset| preset.purpose.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let missing =
                seeded_purposes(provider).difference(&preset_purposes).copied().collect::<Vec<_>>();
            assert!(
                missing.is_empty(),
                "{} seeds models for {missing:?} but its bootstrap presets skip them",
                provider.kind
            );
        }
    }

    #[test]
    fn bootstrap_presets_name_models_their_provider_actually_seeds() {
        for provider in PROVIDERS {
            for preset in provider.bootstrap_presets {
                let model = provider
                    .models
                    .iter()
                    .find(|model| model.model_name == preset.model_name)
                    .unwrap_or_else(|| {
                        panic!(
                            "{} preset names {} which it does not seed",
                            provider.kind, preset.model_name
                        )
                    });
                assert!(
                    model.binding_purposes.contains(&preset.purpose),
                    "{} binds {} to {} which that model cannot serve",
                    provider.kind,
                    preset.model_name,
                    preset.purpose.as_str()
                );
            }
        }
    }

    #[test]
    fn every_provider_either_serves_a_purpose_or_declares_why_it_cannot() {
        // Silence is the failure mode this replaces: before the declaration a
        // provider simply had no embedding model, and the operator learned
        // that from a failed ingest rather than from the setup screen.
        for provider in PROVIDERS.iter().filter(|provider| !provider.models.is_empty()) {
            let served = seeded_purposes(provider);
            let declared = provider
                .purpose_gaps
                .iter()
                .map(|gap| gap.purpose.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            for purpose in [
                "extract_text",
                "extract_graph",
                "graph_entity_type",
                "embed_chunk",
                "query_compile",
                "query_rerank",
                "query_answer",
                "agent",
            ] {
                assert!(
                    served.contains(purpose) || declared.contains(purpose),
                    "{} neither serves {purpose} nor declares why it cannot",
                    provider.kind
                );
            }
        }
    }

    #[test]
    fn declared_gaps_carry_a_localisable_code() {
        // The first version shipped only English prose, which rendered
        // untranslated on a Russian screen. A UI needs something stable to
        // key a translation off.
        let mut codes = std::collections::BTreeSet::new();
        for provider in PROVIDERS {
            for gap in provider.purpose_gaps {
                assert!(
                    !gap.code.trim().is_empty(),
                    "{} declares {} without a code to translate",
                    provider.kind,
                    gap.purpose.as_str()
                );
                assert!(
                    gap.code.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                    "{} uses a code that is not a stable identifier: {}",
                    provider.kind,
                    gap.code
                );
                codes.insert(gap.code);
            }
        }
        // Same cause, same code: a per-provider code would multiply the
        // translation surface for no gain. The decision-only provider adds two
        // causes of its own (IRG-182): a capability it lacks, and a runtime
        // that has not shipped.
        assert!(codes.len() <= 6, "declared-gap codes should stay a small set: {codes:?}");
    }

    #[test]
    fn declared_gaps_carry_a_reason_and_do_not_contradict_a_seeded_model() {
        for provider in PROVIDERS {
            let served = seeded_purposes(provider);
            for gap in provider.purpose_gaps {
                assert!(
                    !gap.reason.trim().is_empty(),
                    "{} declares {} unsupported without a reason",
                    provider.kind,
                    gap.purpose.as_str()
                );
                // A declared gap must never sit on a purpose a seeded model
                // serves: that combination would advertise a capability the
                // catalog contradicts.
                assert!(
                    !served.contains(gap.purpose.as_str()),
                    "{} declares {} unsupported while seeding a model for it",
                    provider.kind,
                    gap.purpose.as_str()
                );
            }
        }
    }

    #[test]
    fn declared_gaps_survive_the_round_trip_into_the_typed_profile() {
        // The seed wrote these into capability_flags_json and the API dropped
        // them on the way out, so the UI could not show what the catalog knew.
        // Parsing the seeded row back through the same type the HTTP layer
        // uses is what proves the field actually reaches a caller.
        for provider in PROVIDERS.iter().filter(|p| !p.purpose_gaps.is_empty()) {
            let published = provider_profile(provider);
            let parsed: crate::domains::provider_profiles::ProviderProfile =
                serde_json::from_value(published).unwrap_or_else(|error| {
                    panic!("{} profile must parse as ProviderProfile: {error}", provider.kind)
                });
            assert_eq!(
                parsed.purpose_gaps.len(),
                provider.purpose_gaps.len(),
                "{} declares {} gaps but the parsed profile carries {}",
                provider.kind,
                provider.purpose_gaps.len(),
                parsed.purpose_gaps.len()
            );
            assert!(
                parsed.purpose_gaps.iter().all(|gap| !gap.reason.trim().is_empty()),
                "{} loses its gap reasons in the round trip",
                provider.kind
            );
        }
    }

    #[test]
    fn provider_rows_publish_their_declared_gaps() {
        let anthropic = provider_seed("anthropic").expect("anthropic seed must exist");
        let published = provider_profile(anthropic);
        let gaps = published["purposeGaps"].as_array().expect("array");
        let declared =
            gaps.iter().map(|gap| gap["purpose"].as_str().expect("purpose")).collect::<Vec<_>>();
        assert_eq!(declared, ["embed_chunk", "extract_graph"]);
        assert!(
            gaps.iter().all(|gap| !gap["reason"].as_str().expect("reason").trim().is_empty()),
            "every published gap must carry the reason a user can act on"
        );
    }

    #[test]
    fn an_operator_supplied_gap_only_sits_on_a_provider_whose_models_can_be_listed() {
        // The two kinds promise different things to the operator. "You choose
        // the model" is only actionable if the binding screen can show which
        // models exist, and that comes from the provider's own listing
        // endpoint. Declaring it on a provider that publishes no listing would
        // leave the same empty dropdown the kind exists to explain.
        for provider in PROVIDERS {
            let operator_supplied = provider
                .purpose_gaps
                .iter()
                .filter(|gap| gap.kind == PurposeGapKind::OperatorSupplies)
                .count();
            if operator_supplied == 0 {
                continue;
            }
            let published = provider_profile(provider);
            assert_eq!(
                published["modelDiscovery"]["mode"].as_str(),
                Some("credential"),
                "{} says the operator supplies its models but publishes no listing to choose from",
                provider.kind
            );
        }
    }

    #[test]
    fn a_gap_kind_reaches_the_typed_profile() {
        // Kind and code were one field once, and a provider read as "not
        // supported" for chat it serves perfectly well. The kind is only
        // useful if it survives into what the UI parses. GPTunnel declares
        // operator-supplied chat because it publishes no prices; OpenRouter
        // and RouterAI now seed a priced chat set, so their gaps are gone.
        let router = provider_seed("gptunnel").expect("gptunnel seed must exist");
        let parsed: crate::domains::provider_profiles::ProviderProfile =
            serde_json::from_value(provider_profile(router)).expect("profile must parse");
        assert!(
            parsed.purpose_gaps.iter().all(|gap| {
                gap.kind
                    == crate::domains::provider_profiles::ProviderPurposeGapKind::OperatorSupplies
            }),
            "a router's gaps are all the operator's choice of model, not a capability it lacks"
        );
        let anthropic = provider_seed("anthropic").expect("anthropic seed must exist");
        let parsed: crate::domains::provider_profiles::ProviderProfile =
            serde_json::from_value(provider_profile(anthropic)).expect("profile must parse");
        assert!(
            parsed.purpose_gaps.iter().all(|gap| {
                gap.kind
                    == crate::domains::provider_profiles::ProviderPurposeGapKind::ProviderCannot
            }),
            "a vendor with no embeddings endpoint has a capability gap, not an unpicked model"
        );
    }

    #[test]
    fn a_self_hosted_runtime_is_not_checked_against_when_its_account_is_saved() {
        // There is no credential to check, and the runtime is frequently not
        // up yet when its account is created — a clean stack configures the
        // provider before starting the model server. Listing models on save
        // would refuse the account with nothing wrong with it, which is also
        // the "clone it and run on an empty stack" promise this repository
        // makes.
        for provider in PROVIDERS {
            let published = provider_profile(provider);
            let self_hosted = published["baseUrl"]["allowPrivateNetwork"] == true
                && published["credentials"]["apiKeyRequired"] == false;
            if !self_hosted {
                continue;
            }
            assert_eq!(
                published["credentials"]["validationMode"].as_str(),
                Some("none"),
                "{} is the operator's own runtime and must not be contacted to save its account",
                provider.kind
            );
            assert_eq!(
                published["modelDiscovery"]["mode"].as_str(),
                Some("credential"),
                "{} still has to list its models for the binding screen",
                provider.kind
            );
        }
    }

    #[test]
    fn openai_manifest_refreshes_the_frontier_generation_and_long_context_tiers() {
        let openai = provider_seed("openai").expect("openai seed must exist");
        assert_openai_astra_frontier_tier(openai);
        for (name, short, long) in [
            ("gpt-6-sol", ("2", "0.2", "2.5", "10"), ("4", "0.4", "15")),
            ("gpt-6-luna", ("0.1", "0.01", "0.125", "0.5"), ("0.2", "0.02", "0.75")),
        ] {
            assert_openai_gpt6_tier_model(openai, name, short, long);
        }
        assert_openai_previous_generation_rates(openai);
        assert!(openai.models.iter().any(|model| model.model_name == "gpt-5.4-mini"));
    }

    fn openai_model<'a>(openai: &'a ProviderSeed, name: &str) -> &'a ModelSeed {
        openai
            .models
            .iter()
            .find(|model| model.model_name == name)
            .unwrap_or_else(|| panic!("{name} must be part of the current OpenAI generation"))
    }

    /// The frontier `gpt-6-astra` row carries the multimodal window and the
    /// short/long rate pair the manifest publishes for it.
    fn assert_openai_astra_frontier_tier(openai: &ProviderSeed) {
        let astra = openai_model(openai, "gpt-6-astra");
        assert_eq!(astra.observed_input_modalities, TEXT_AND_IMAGE);
        assert_eq!(astra.context_window, Some(1_050_000));
        assert_eq!(astra.max_output_tokens, Some(128_000));
        assert_eq!(
            (
                astra.pricing.input,
                astra.pricing.cached_input,
                astra.pricing.cache_write_input,
                astra.pricing.output,
            ),
            ("10", "1", "12.5", "50")
        );
        let tier = astra.pricing.long_context.expect("astra has a long-context tier");
        assert_eq!(tier.threshold_tokens, 272_000);
        assert_eq!((tier.input, tier.cached_input, tier.output), ("20", "2", "75"));
    }

    /// A `gpt-6` tier row shares the frontier window but carries its own short
    /// and long-context rate pair.
    fn assert_openai_gpt6_tier_model(
        openai: &ProviderSeed,
        name: &str,
        short: (&str, &str, &str, &str),
        long: (&str, &str, &str),
    ) {
        let seeded = openai_model(openai, name);
        assert_eq!(seeded.observed_input_modalities, TEXT_AND_IMAGE);
        assert_eq!(
            (seeded.context_window, seeded.max_output_tokens),
            (Some(1_050_000), Some(128_000))
        );
        assert_eq!(
            (
                seeded.pricing.input,
                seeded.pricing.cached_input,
                seeded.pricing.cache_write_input,
                seeded.pricing.output,
            ),
            short,
            "{name} short-context rates"
        );
        let tier = seeded.pricing.long_context.expect("GPT-6 tiers carry a long-context rate");
        assert_eq!(tier.threshold_tokens, 272_000);
        assert_eq!((tier.input, tier.cached_input, tier.output), long, "{name} long-context rates");
    }

    /// The previous generation keeps its own rates: `gpt-5.6-sol` bills a
    /// long-context tier, `gpt-5.6-cyber` bills none.
    fn assert_openai_previous_generation_rates(openai: &ProviderSeed) {
        let sol = openai_model(openai, "gpt-5.6-sol");
        assert_eq!(
            (
                sol.pricing.input,
                sol.pricing.cached_input,
                sol.pricing.cache_write_input,
                sol.pricing.output,
            ),
            ("4", "0.4", "5", "20")
        );
        let tier = sol.pricing.long_context.expect("sol has a long-context tier");
        assert_eq!(tier.threshold_tokens, 272_000);
        assert_eq!((tier.input, tier.cached_input, tier.output), ("8", "0.8", "30"));

        let cyber = openai_model(openai, "gpt-5.6-cyber");
        assert_eq!(
            (
                cyber.pricing.input,
                cyber.pricing.cached_input,
                cyber.pricing.cache_write_input,
                cyber.pricing.output,
            ),
            ("12.5", "1.25", "15.625", "75")
        );
        assert!(cyber.pricing.long_context.is_none());
    }

    #[test]
    fn anthropic_manifest_carries_only_current_generation_ids() {
        let anthropic = provider_seed("anthropic").expect("anthropic seed must exist");
        let ids = anthropic.models.iter().map(|model| model.model_name).collect::<Vec<_>>();
        assert_eq!(
            ids,
            ["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5-20251001"]
        );
        assert!(!ids.contains(&"claude-opus-4-8"), "retired Opus id must not be seeded");
        assert!(!ids.contains(&"claude-fable-5"), "the fable-5 alias is superseded by fable-5-1");

        let fable = anthropic
            .models
            .iter()
            .find(|model| model.model_name == "claude-fable-5-1")
            .expect("fable-5-1 seed");
        // Fable 5.1 is the cache-read exception: 0.025x input, not the family's
        // 0.1x.
        assert_eq!(fable.pricing.cached_input, "0.25");
        assert_eq!(fable.context_window, Some(1_000_000));

        let haiku = anthropic
            .models
            .iter()
            .find(|model| model.model_name == "claude-haiku-4-5-20251001")
            .expect("haiku seed");
        assert_eq!((haiku.context_window, haiku.max_output_tokens), (Some(200_000), Some(64_000)));
    }

    #[test]
    fn router_manifests_seed_a_priced_chat_set_in_their_listing_currency() {
        let expected = [
            "openai/gpt-6-astra",
            "openai/gpt-6-sol",
            "openai/gpt-6-luna",
            "openai/gpt-5.6-sol",
            "openai/gpt-5.4-mini",
            "anthropic/claude-fable-5.1",
            "anthropic/claude-opus-5",
            "anthropic/claude-sonnet-5",
            "anthropic/claude-haiku-4.5",
            "google/gemini-3.1-pro-preview",
            "google/gemini-3.6-flash",
            "deepseek/deepseek-v4.1-flash",
            "deepseek/deepseek-v4-pro-0813",
            "qwen/qwen3.8-max-0902",
            "qwen/qwen3.8-flash",
            "minimax/minimax-m3",
            "x-ai/grok-4.6",
            "mistralai/mistral-large-2512",
        ];
        for (kind, currency) in [("openrouter", "USD"), ("routerai", "RUB")] {
            let provider = provider_seed(kind).expect("router seed must exist");
            let chat = provider
                .models
                .iter()
                .filter(|model| !model.binding_purposes.contains(&BindingPurpose::EmbedChunk))
                .collect::<Vec<_>>();
            assert_eq!(
                chat.iter().map(|model| model.model_name).collect::<Vec<_>>(),
                expected,
                "{kind} chat set"
            );
            assert!(
                chat.iter().all(|model| {
                    model.pricing.input.parse::<f64>().unwrap() > 0.0
                        && model.pricing.output.parse::<f64>().unwrap() > 0.0
                }),
                "{kind} chat rows must carry the listing's real rates"
            );
            assert_eq!(
                provider_profile(provider)["modelDiscovery"]["listedPricingCurrency"].as_str(),
                Some(currency),
                "{kind} currency"
            );
        }
        // Both routers list the OpenAI embedding ids with prices, so each
        // serves every purpose and leaves no declared gap.
        for kind in ["openrouter", "routerai"] {
            let provider = provider_seed(kind).expect("router seed must exist");
            assert!(provider.purpose_gaps.is_empty(), "{kind} must declare no purpose gap");
            let embeddings = provider
                .models
                .iter()
                .filter(|model| model.binding_purposes.contains(&BindingPurpose::EmbedChunk))
                .collect::<Vec<_>>();
            assert_eq!(
                embeddings.iter().map(|model| model.model_name).collect::<Vec<_>>(),
                ["openai/text-embedding-3-large", "openai/text-embedding-3-small"],
                "{kind} embedding set"
            );
            assert!(
                embeddings.iter().all(|model| {
                    model.pricing.input.parse::<f64>().unwrap() > 0.0
                        && model.embedding_dimensions.is_some()
                }),
                "{kind} embedding rows must carry the listing's real input rate and a width"
            );
        }

        // RouterAI is the one that changed: its embedding rows quote roubles
        // like its chat rows, not the dollar list price OpenRouter carries.
        let routerai_embeddings = provider_seed("routerai")
            .expect("routerai seed")
            .models
            .iter()
            .filter(|model| model.binding_purposes.contains(&BindingPurpose::EmbedChunk))
            .collect::<Vec<_>>();
        assert_eq!(
            routerai_embeddings
                .iter()
                .map(|model| (model.model_name, model.pricing.input))
                .collect::<Vec<_>>(),
            [("openai/text-embedding-3-large", "14.23"), ("openai/text-embedding-3-small", "2.19")]
        );
    }

    #[test]
    fn qwen_and_litellm_extend_the_hosted_catalog() {
        assert_qwen_hosted_catalog();
        assert_litellm_gateway_catalog();
    }

    /// DashScope's compatible endpoint seeds the current Qwen chat and vision
    /// ids and declares only its embedding gap, with the operator choosing the
    /// model.
    fn assert_qwen_hosted_catalog() {
        let qwen = provider_seed("qwen").expect("qwen seed must exist");
        assert_eq!(qwen.api_style, "openai_compatible");
        assert_eq!(
            qwen.default_base_url,
            Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1")
        );
        assert_eq!(
            qwen.models.iter().map(|model| model.model_name).collect::<Vec<_>>(),
            [
                "qwen3.8-max",
                "qwen3.8-flash",
                "qwen3.7-plus",
                "qwen3.7-flash",
                "qwen3-coder-plus",
                "qwen3-coder-flash",
                "qwen3-vl-plus",
                "qwen3-vl-flash",
            ]
        );
        assert_eq!(qwen.purpose_gaps.len(), 1);
        assert_eq!(qwen.purpose_gaps[0].purpose.as_str(), "embed_chunk");
        assert_eq!(qwen.purpose_gaps[0].kind.as_str(), "operator_supplies");
    }

    /// LiteLLM seeds no models of its own — the operator supplies every one —
    /// and its profile demands a base URL, lists models on save and publishes
    /// no currency because it prices nothing.
    fn assert_litellm_gateway_catalog() {
        let litellm = provider_seed("litellm").expect("litellm seed must exist");
        assert!(litellm.models.is_empty(), "the gateway's models are the operator's");
        assert_eq!(litellm.purpose_gaps.len(), 8);
        let profile = provider_profile(litellm);
        assert_eq!(profile["credentials"]["apiKeyRequired"], true);
        assert_eq!(profile["credentials"]["baseUrlMode"], "required");
        assert_eq!(profile["credentials"]["baseUrlRequired"], true);
        assert_eq!(profile["modelDiscovery"]["mode"], "credential");
        assert_eq!(profile["modelDiscovery"]["listedPricingCurrency"], serde_json::Value::Null);
        assert_eq!(profile["requestPolicy"]["sampling"], "forward");
        assert_eq!(profile["runtime"]["structuredOutput"], "prompt_only_json_object");
    }

    #[test]
    fn gemini_manifest_carries_current_direct_api_models_and_rates() {
        let gemini = provider_seed("gemini").expect("Gemini provider seed must exist");
        assert_gemini_manifest_ids(gemini);
        assert_gemini_chat_rates(gemini);
        assert_gemini_current_embedding_family(gemini);
    }

    fn gemini_model<'a>(gemini: &'a ProviderSeed, name: &str) -> &'a ModelSeed {
        gemini.models.iter().find(|model| model.model_name == name).unwrap_or_else(|| {
            panic!("{name} must be part of the current direct Gemini API generation")
        })
    }

    /// The stale 2.5 chat ids are gone; the current generation replaces them.
    fn assert_gemini_manifest_ids(gemini: &ProviderSeed) {
        assert!(
            gemini.models.iter().all(|model| !model.model_name.starts_with("gemini-2.5-")),
            "superseded 2.5 chat ids must not share the manifest with the current set"
        );
        assert_eq!(
            gemini.models.iter().map(|model| model.model_name).collect::<Vec<_>>(),
            [
                "gemini-3.1-pro-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
                "gemini-embedding-001",
                "gemini-embedding-2",
            ]
        );
    }

    /// The three chat rows carry their observed windows and per-model rates;
    /// only Pro bills a long-context tier.
    fn assert_gemini_chat_rates(gemini: &ProviderSeed) {
        let pro = gemini_model(gemini, "gemini-3.1-pro-preview");
        assert_eq!(pro.observed_input_modalities, TEXT_AND_IMAGE);
        assert_eq!((pro.context_window, pro.max_output_tokens), (Some(1_048_576), Some(65_536)));
        assert_eq!(
            (pro.pricing.input, pro.pricing.cached_input, pro.pricing.output),
            ("2", "0.2", "12")
        );
        let tier = pro.pricing.long_context.expect("Pro bills more above 200K tokens");
        assert_eq!(tier.threshold_tokens, 200_000);
        assert_eq!((tier.input, tier.cached_input, tier.output), ("4", "0.4", "18"));

        let flash = gemini_model(gemini, "gemini-3.8-flash");
        assert_eq!(
            (flash.context_window, flash.max_output_tokens),
            (Some(1_048_576), Some(65_536))
        );
        assert_eq!(
            (flash.pricing.input, flash.pricing.cached_input, flash.pricing.output),
            ("0.75", "0.075", "3.75")
        );
        assert!(flash.pricing.long_context.is_none(), "Flash bills no long-context tier");

        let lite = gemini_model(gemini, "gemini-3.5-flash-lite");
        assert_eq!(
            (lite.pricing.input, lite.pricing.cached_input, lite.pricing.output),
            ("0.3", "0.03", "2.5")
        );
    }

    /// The current embedding family: 001 stays for text-only workloads, 002 is
    /// the multimodal successor; both at the native Matryoshka width.
    fn assert_gemini_current_embedding_family(gemini: &ProviderSeed) {
        let embedding = gemini_model(gemini, "gemini-embedding-001");
        assert_eq!(embedding.embedding_dimensions, Some(3_072));
        assert!(embedding.binding_purposes.contains(&BindingPurpose::EmbedChunk));
        let multimodal_embedding = gemini_model(gemini, "gemini-embedding-2");
        assert_eq!(multimodal_embedding.embedding_dimensions, Some(3_072));
        assert_eq!(multimodal_embedding.pricing.input, "0.20");
        assert!(multimodal_embedding.binding_purposes.contains(&BindingPurpose::EmbedChunk));
    }

    #[test]
    fn stale_seed_owned_rows_are_retired_while_manifest_and_operator_rows_survive() {
        let id = |value: u128| Uuid::from_u128(value);
        let existing = [
            CatalogModelIdentity {
                id: id(1),
                provider_kind: "anthropic".to_string(),
                model_name: "claude-opus-4-8".to_string(),
                seed_owned: true,
            },
            CatalogModelIdentity {
                id: id(2),
                provider_kind: "anthropic".to_string(),
                model_name: "claude-opus-5".to_string(),
                seed_owned: true,
            },
            CatalogModelIdentity {
                id: id(3),
                provider_kind: "openai".to_string(),
                model_name: "operator-fine-tune".to_string(),
                seed_owned: false,
            },
        ];
        let manifest = BTreeSet::from([("anthropic".to_string(), "claude-opus-5".to_string())]);

        assert_eq!(
            seed_owned_rows_absent_from_manifest(&existing, &manifest),
            vec![id(1)],
            "only the seed-owned row missing from the manifest is retired; the row still in \
             the manifest and the operator's own row survive"
        );
    }

    #[test]
    fn seed_ids_are_deterministic() {
        assert_eq!(
            seed_uuid(&["ironrag", "anthropic", "provider"]),
            seed_uuid(&["ironrag", "anthropic", "provider"])
        );
    }

    /// TypeSafe Jev (IRG-182) seeds a typed decision provider an operator opts
    /// into: no bootstrap preset, no account and no binding, so it cannot
    /// touch an existing deployment's runtime, and `model_list` credential
    /// validation because it exposes no chat round trip to probe. The
    /// decision-capability invariant in `parse_provider_profile` is what keeps
    /// this shape honest.
    #[test]
    fn typesafe_jev_seeds_as_a_decision_provider_without_presets_or_bindings() {
        let provider = provider_seed("typesafe").expect("TypeSafe Jev seed must exist");
        assert_typesafe_jev_identity(provider);
        let published = provider_profile(provider);
        assert_typesafe_jev_published_shape(&published);
        let parsed: crate::domains::provider_profiles::ProviderProfile =
            serde_json::from_value(published).expect("TypeSafe Jev profile must parse");
        assert_typesafe_jev_parsed_profile(&parsed);
        assert_typesafe_jev_decision_model(provider);
        assert_typesafe_jev_declared_gaps(provider, &parsed);
    }

    /// The seed's own row: a preview decision runtime with no bootstrap
    /// preset, so it cannot touch an existing deployment's runtime.
    fn assert_typesafe_jev_identity(provider: &ProviderSeed) {
        assert_eq!(provider.display_name, "TypeSafe Jev");
        assert_eq!(provider.api_style, "typesafe_system_one");
        assert_eq!(provider.lifecycle_state, "preview");
        assert_eq!(provider.default_base_url, Some("https://api.typesafe.ai"));
        assert!(
            provider.bootstrap_presets.is_empty(),
            "an opt-in decision provider must not seed a bootstrap preset"
        );
    }

    /// The published JSON: a bearer `typesafe_system_one` profile with only
    /// the decision discovery path, `model_list` validation and every
    /// generative capability unsupported.
    fn assert_typesafe_jev_published_shape(published: &serde_json::Value) {
        assert_eq!(published["bootstrapPresets"], serde_json::json!([]));
        assert_eq!(published["runtime"]["kind"], "typesafe_system_one");
        assert_eq!(published["runtime"]["authScheme"], "bearer");
        assert_eq!(published["runtime"]["chatPath"], "/v1/systemone");
        assert_eq!(published["runtime"]["structuredOutput"], "unsupported");
        assert_eq!(published["credentials"]["apiKeyRequired"], true);
        assert_eq!(published["credentials"]["baseUrlMode"], "fixed");
        assert_eq!(
            published["credentials"]["validationMode"], "model_list",
            "TypeSafe exposes no chat round trip, so credential validation lists its models"
        );
        assert_eq!(
            published["modelDiscovery"]["paths"],
            serde_json::json!([{"capabilityKind": "decision", "path": "/v1/models"}])
        );
        assert_eq!(published["capabilities"]["decision"], "supported");
        assert_eq!(published["capabilities"]["modelDiscovery"], "supported");
        assert_typesafe_jev_generative_capabilities_unsupported(published);
    }

    /// Every generative capability is `unsupported` on the published decision
    /// provider, so the UI never offers one.
    fn assert_typesafe_jev_generative_capabilities_unsupported(published: &serde_json::Value) {
        for generative in ["chat", "embeddings", "vision", "streaming", "tools"] {
            assert_eq!(
                published["capabilities"][generative], "unsupported",
                "a decision provider must mark {generative} unsupported"
            );
        }
    }

    /// The typed profile the HTTP layer parses keeps its single decision path.
    fn assert_typesafe_jev_parsed_profile(
        parsed: &crate::domains::provider_profiles::ProviderProfile,
    ) {
        use crate::domains::provider_profiles::{
            ProviderCapabilityState, ProviderModelCapabilityKind,
        };

        assert_eq!(parsed.capabilities.decision, ProviderCapabilityState::Supported);
        assert_eq!(parsed.model_discovery.paths.len(), 1);
        assert_eq!(
            parsed.model_discovery.paths[0].capability_kind,
            ProviderModelCapabilityKind::Decision
        );
    }

    /// Jev seeds exactly its one preview decision model, allowing only the two
    /// decision purposes and pricing it at zero.
    fn assert_typesafe_jev_decision_model(provider: &ProviderSeed) {
        assert_eq!(provider.models.len(), 1, "TypeSafe seeds exactly its one decision model");
        let model = &provider.models[0];
        assert_eq!(model.model_name, "jev-latest");
        assert_eq!(model.lifecycle_state, "preview");
        assert_eq!(
            model.binding_purposes.iter().map(|purpose| purpose.as_str()).collect::<Vec<_>>(),
            ["graph_entity_type", "query_rerank"],
            "jev-latest allows exactly the two decision purposes"
        );
        assert_eq!(model.pricing.input, "0");
        assert_eq!(model.pricing.output, "0");
    }

    /// Six declared `provider_cannot` gaps, and neither live systemone
    /// decision purpose carries one.
    fn assert_typesafe_jev_declared_gaps(
        provider: &ProviderSeed,
        parsed: &crate::domains::provider_profiles::ProviderProfile,
    ) {
        let cannot = provider
            .purpose_gaps
            .iter()
            .filter(|gap| gap.kind == PurposeGapKind::ProviderCannot)
            .count();
        assert_eq!(cannot, 6);
        assert!(
            parsed.purpose_gaps.iter().all(|gap| gap.purpose.as_str() != "query_rerank"
                && gap.purpose.as_str() != "graph_entity_type"),
            "both systemone decision purposes are live and must carry no gap"
        );
    }
}
