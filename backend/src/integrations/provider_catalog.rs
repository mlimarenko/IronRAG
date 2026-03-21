use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    app::config::Settings,
    domains::provider_profiles::{
        EffectiveProviderProfile, ProviderModelSelection, RuntimeProviderProfileDefaults,
        SupportedProviderKind,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedProviderCatalogEntry {
    pub provider_kind: SupportedProviderKind,
    pub supported_capabilities: Vec<String>,
    pub default_models: BTreeMap<String, String>,
    pub available_models: BTreeMap<String, Vec<String>>,
    pub is_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePricingTarget {
    pub role: String,
    pub provider_kind: SupportedProviderKind,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogPricingSeedDefinition {
    pub provider_kind: SupportedProviderKind,
    pub model_name: &'static str,
    pub capability: &'static str,
    pub billing_unit: &'static str,
    pub input_price: Option<&'static str>,
    pub output_price: Option<&'static str>,
    pub note: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ProviderModelDefinition {
    model_name: &'static str,
    supports_chat: bool,
    supports_embedding: bool,
    supports_vision: bool,
    input_price: Option<&'static str>,
    output_price: Option<&'static str>,
    note: &'static str,
}

macro_rules! model {
    ($name:expr, chat = $chat:expr, embedding = $embedding:expr, vision = $vision:expr, input = $input:expr, output = $output:expr, note = $note:expr) => {
        ProviderModelDefinition {
            model_name: $name,
            supports_chat: $chat,
            supports_embedding: $embedding,
            supports_vision: $vision,
            input_price: $input,
            output_price: $output,
            note: $note,
        }
    };
}

pub const CAPABILITY_CHAT: &str = "chat";
pub const CAPABILITY_EMBEDDINGS: &str = "embeddings";
pub const CAPABILITY_VISION: &str = "vision";
pub const PRICING_CAPABILITY_INDEXING: &str = "indexing";
pub const PRICING_CAPABILITY_EMBEDDING: &str = "embedding";
pub const PRICING_CAPABILITY_ANSWER: &str = "answer";
pub const PRICING_CAPABILITY_VISION: &str = "vision";
pub const PRICING_CAPABILITY_GRAPH_EXTRACT: &str = "graph_extract";
pub const PRICING_CAPABILITY_QUERY_INTENT: &str = "query_intent";
pub const PRICING_CAPABILITY_RERANK: &str = "rerank";
pub const PRICING_CAPABILITY_GRAPH_SUMMARY: &str = "graph_summary";
pub const PRICING_CAPABILITY_EXTRACTION_RECOVERY: &str = "extraction_recovery";
pub const BILLING_UNIT_PER_1M_INPUT_TOKENS: &str = "per_1m_input_tokens";
pub const BILLING_UNIT_PER_1M_TOKENS: &str = "per_1m_tokens";

pub const ROLE_INDEXING: &str = "indexing";
pub const ROLE_EMBEDDING: &str = "embedding";
pub const ROLE_ANSWER: &str = "answer";
pub const ROLE_VISION: &str = "vision";
pub const ROLE_QUERY_INTENT: &str = "query_intent";
pub const ROLE_RERANK: &str = "rerank";
pub const ROLE_GRAPH_SUMMARY: &str = "graph_summary";
pub const ROLE_EXTRACTION_RECOVERY: &str = "extraction_recovery";

const OPENAI_GPT_54_NOTE: &str = "OpenAI API pricing checked 2026-03-19. Standard uncached text-token rate; GPT-5.4 long-context requests above 272K input tokens bill higher.";
const OPENAI_GPT_54_PRO_NOTE: &str = "OpenAI API pricing checked 2026-03-19. Standard uncached text-token rate; GPT-5.4 Pro long-context requests above 272K input tokens bill higher.";
const OPENAI_GPT_54_MINI_NOTE: &str =
    "OpenAI GPT-5.4 mini model page pricing checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_54_NANO_NOTE: &str =
    "OpenAI GPT-5.4 nano model page pricing checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_5_FAMILY_NOTE: &str = "OpenAI GPT-5, GPT-5.1, and GPT-5.2 model page pricing checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_5_PRO_FAMILY_NOTE: &str = "OpenAI GPT-5 pro model page pricing checked 2026-03-19. Standard uncached text-token rate. OpenAI docs describe these models as Responses-first for advanced multi-turn reasoning.";
const OPENAI_GPT_5_MINI_NOTE: &str =
    "OpenAI GPT-5 mini model page pricing checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_5_NANO_NOTE: &str =
    "OpenAI GPT-5 nano model page pricing checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_41_NOTE: &str =
    "OpenAI GPT-4.1 model pages checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_GPT_4O_NOTE: &str =
    "OpenAI GPT-4o model pages checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_O_SERIES_NOTE: &str =
    "OpenAI o-series model pages checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_O_SERIES_PRO_NOTE: &str = "OpenAI o-series pro model pages checked 2026-03-19. Standard uncached text-token rate. OpenAI docs describe these models as Responses-first for advanced multi-turn reasoning.";
const OPENAI_LEGACY_NOTE: &str = "OpenAI API pricing legacy models section checked 2026-03-19. Standard uncached text-token rate.";
const OPENAI_EMBEDDING_NOTE: &str = "OpenAI API pricing embeddings section checked 2026-03-19.";
const DEEPSEEK_NOTE: &str = "DeepSeek Models & Pricing checked 2026-03-19. Uses cache-miss input pricing; cache-hit input is cheaper.";
const QWEN_INTL_MIN_TIER_NOTE: &str = "Alibaba Cloud Model Studio international deployment pricing checked 2026-03-19. Uses the minimum published tier; larger prompts and cache discounts can change the effective rate.";
const QWEN_INTL_MIN_TIER_MODE_NOTE: &str = "Alibaba Cloud Model Studio international deployment pricing checked 2026-03-19. Uses the minimum published non-thinking tier; thinking mode, larger prompts, and cache discounts can change the effective rate.";
const QWEN_INTL_FIXED_NOTE: &str = "Alibaba Cloud Model Studio international deployment pricing checked 2026-03-19. Uses the published no-tier rate.";
const QWEN_INTL_EMBEDDING_NOTE: &str =
    "Alibaba Cloud Model Studio international deployment embedding pricing checked 2026-03-19.";

const OPENAI_MODELS: &[ProviderModelDefinition] = &[
    model!(
        "gpt-5.4",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("15.00"),
        note = OPENAI_GPT_54_NOTE
    ),
    model!(
        "gpt-5.4-2026-03-05",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("15.00"),
        note = OPENAI_GPT_54_NOTE
    ),
    model!(
        "gpt-5.4-pro",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("30.00"),
        output = Some("180.00"),
        note = OPENAI_GPT_54_PRO_NOTE
    ),
    model!(
        "gpt-5.4-pro-2026-03-05",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("30.00"),
        output = Some("180.00"),
        note = OPENAI_GPT_54_PRO_NOTE
    ),
    model!(
        "gpt-5.4-mini",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.75"),
        output = Some("4.50"),
        note = OPENAI_GPT_54_MINI_NOTE
    ),
    model!(
        "gpt-5.4-mini-2026-03-17",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.75"),
        output = Some("4.50"),
        note = OPENAI_GPT_54_MINI_NOTE
    ),
    model!(
        "gpt-5.4-nano",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.20"),
        output = Some("1.25"),
        note = OPENAI_GPT_54_NANO_NOTE
    ),
    model!(
        "gpt-5.4-nano-2026-03-17",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.20"),
        output = Some("1.25"),
        note = OPENAI_GPT_54_NANO_NOTE
    ),
    model!(
        "gpt-5.2",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.75"),
        output = Some("14.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5.2-2025-12-11",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.75"),
        output = Some("14.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5.2-pro",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("21.00"),
        output = Some("168.00"),
        note = OPENAI_GPT_5_PRO_FAMILY_NOTE
    ),
    model!(
        "gpt-5.2-pro-2025-12-11",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("21.00"),
        output = Some("168.00"),
        note = OPENAI_GPT_5_PRO_FAMILY_NOTE
    ),
    model!(
        "gpt-5.1",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.25"),
        output = Some("10.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5.1-2025-11-13",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.25"),
        output = Some("10.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.25"),
        output = Some("10.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5-2025-08-07",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.25"),
        output = Some("10.00"),
        note = OPENAI_GPT_5_FAMILY_NOTE
    ),
    model!(
        "gpt-5-pro",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("15.00"),
        output = Some("120.00"),
        note = OPENAI_GPT_5_PRO_FAMILY_NOTE
    ),
    model!(
        "gpt-5-pro-2025-10-06",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("15.00"),
        output = Some("120.00"),
        note = OPENAI_GPT_5_PRO_FAMILY_NOTE
    ),
    model!(
        "gpt-5-mini",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.25"),
        output = Some("2.00"),
        note = OPENAI_GPT_5_MINI_NOTE
    ),
    model!(
        "gpt-5-mini-2025-08-07",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.25"),
        output = Some("2.00"),
        note = OPENAI_GPT_5_MINI_NOTE
    ),
    model!(
        "gpt-5-nano",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.05"),
        output = Some("0.40"),
        note = OPENAI_GPT_5_NANO_NOTE
    ),
    model!(
        "gpt-5-nano-2025-08-07",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.05"),
        output = Some("0.40"),
        note = OPENAI_GPT_5_NANO_NOTE
    ),
    model!(
        "gpt-4.1",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.00"),
        output = Some("8.00"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4.1-2025-04-14",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.00"),
        output = Some("8.00"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4.1-mini",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.40"),
        output = Some("1.60"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4.1-mini-2025-04-14",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.40"),
        output = Some("1.60"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4.1-nano",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.10"),
        output = Some("0.40"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4.1-nano-2025-04-14",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.10"),
        output = Some("0.40"),
        note = OPENAI_GPT_41_NOTE
    ),
    model!(
        "gpt-4o",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("10.00"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "gpt-4o-2024-11-20",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("10.00"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "gpt-4o-2024-08-06",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("10.00"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "gpt-4o-2024-05-13",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.50"),
        output = Some("10.00"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "gpt-4o-mini",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.15"),
        output = Some("0.60"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "gpt-4o-mini-2024-07-18",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.15"),
        output = Some("0.60"),
        note = OPENAI_GPT_4O_NOTE
    ),
    model!(
        "o3",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.00"),
        output = Some("8.00"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o3-2025-04-16",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("2.00"),
        output = Some("8.00"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o3-mini",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o3-mini-2025-01-31",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o3-pro",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("20.00"),
        output = Some("80.00"),
        note = OPENAI_O_SERIES_PRO_NOTE
    ),
    model!(
        "o3-pro-2025-06-10",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("20.00"),
        output = Some("80.00"),
        note = OPENAI_O_SERIES_PRO_NOTE
    ),
    model!(
        "o4-mini",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o4-mini-2025-04-16",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o1",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("15.00"),
        output = Some("60.00"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o1-2024-12-17",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("15.00"),
        output = Some("60.00"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o1-mini",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o1-mini-2024-09-12",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.10"),
        output = Some("4.40"),
        note = OPENAI_O_SERIES_NOTE
    ),
    model!(
        "o1-pro",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("150.00"),
        output = Some("600.00"),
        note = OPENAI_O_SERIES_PRO_NOTE
    ),
    model!(
        "o1-pro-2025-03-19",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("150.00"),
        output = Some("600.00"),
        note = OPENAI_O_SERIES_PRO_NOTE
    ),
    model!(
        "chatgpt-4o-latest",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("5.00"),
        output = Some("15.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-turbo-2024-04-09",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("10.00"),
        output = Some("30.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-0125-preview",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("10.00"),
        output = Some("30.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-1106-preview",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("10.00"),
        output = Some("30.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-1106-vision-preview",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("10.00"),
        output = Some("30.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-0613",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("30.00"),
        output = Some("60.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-0314",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("30.00"),
        output = Some("60.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-4-32k",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("60.00"),
        output = Some("120.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-turbo",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.50"),
        output = Some("1.50"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-turbo-0125",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.50"),
        output = Some("1.50"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-turbo-1106",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.00"),
        output = Some("2.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-turbo-0613",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.50"),
        output = Some("2.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-0301",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.50"),
        output = Some("2.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "gpt-3.5-turbo-16k-0613",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("3.00"),
        output = Some("4.00"),
        note = OPENAI_LEGACY_NOTE
    ),
    model!(
        "text-embedding-3-large",
        chat = false,
        embedding = true,
        vision = false,
        input = Some("0.13"),
        output = None,
        note = OPENAI_EMBEDDING_NOTE
    ),
    model!(
        "text-embedding-3-small",
        chat = false,
        embedding = true,
        vision = false,
        input = Some("0.02"),
        output = None,
        note = OPENAI_EMBEDDING_NOTE
    ),
    model!(
        "text-embedding-ada-002",
        chat = false,
        embedding = true,
        vision = false,
        input = Some("0.10"),
        output = None,
        note = OPENAI_EMBEDDING_NOTE
    ),
];

const DEEPSEEK_MODELS: &[ProviderModelDefinition] = &[
    model!(
        "deepseek-chat",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.28"),
        output = Some("0.42"),
        note = DEEPSEEK_NOTE
    ),
    model!(
        "deepseek-reasoner",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.28"),
        output = Some("0.42"),
        note = DEEPSEEK_NOTE
    ),
];

const QWEN_MODELS: &[ProviderModelDefinition] = &[
    model!(
        "qwen3-max",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.2"),
        output = Some("6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-max-2026-01-23",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.2"),
        output = Some("6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-max-2025-09-23",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.2"),
        output = Some("6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-max-preview",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.2"),
        output = Some("6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3.5-plus",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("2.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3.5-plus-2026-02-15",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("2.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen-plus",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("1.2"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-plus-latest",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("1.2"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-plus-2025-12-01",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("1.2"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-plus-2025-09-11",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("1.2"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-plus-2025-07-28",
        chat = true,
        embedding = false,
        vision = true,
        input = Some("0.4"),
        output = Some("1.2"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen3.5-flash",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.1"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3.5-flash-2026-02-23",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.1"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen-flash",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.05"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-flash-2025-07-28",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.05"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_MODE_NOTE
    ),
    model!(
        "qwen-max",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.6"),
        output = Some("6.4"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-max-latest",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.6"),
        output = Some("6.4"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-max-2025-01-25",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1.6"),
        output = Some("6.4"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen3-coder-plus",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1"),
        output = Some("5"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-coder-plus-2025-09-23",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1"),
        output = Some("5"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-coder-plus-2025-07-22",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("1"),
        output = Some("5"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-coder-flash",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.3"),
        output = Some("1.5"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-coder-flash-2025-07-28",
        chat = true,
        embedding = false,
        vision = false,
        input = Some("0.3"),
        output = Some("1.5"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-plus",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.2"),
        output = Some("1.6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-plus-2025-12-19",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.2"),
        output = Some("1.6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-plus-2025-09-23",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.2"),
        output = Some("1.6"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-flash",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.05"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-flash-2026-01-22",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.05"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen3-vl-flash-2025-10-15",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.05"),
        output = Some("0.4"),
        note = QWEN_INTL_MIN_TIER_NOTE
    ),
    model!(
        "qwen-vl-max",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.8"),
        output = Some("3.2"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-vl-max-latest",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.8"),
        output = Some("3.2"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-vl-max-2025-08-13",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.8"),
        output = Some("3.2"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-vl-plus",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.21"),
        output = Some("0.63"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-vl-plus-latest",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.21"),
        output = Some("0.63"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "qwen-vl-plus-2025-08-15",
        chat = false,
        embedding = false,
        vision = true,
        input = Some("0.21"),
        output = Some("0.63"),
        note = QWEN_INTL_FIXED_NOTE
    ),
    model!(
        "text-embedding-v4",
        chat = false,
        embedding = true,
        vision = false,
        input = Some("0.07"),
        output = None,
        note = QWEN_INTL_EMBEDDING_NOTE
    ),
    model!(
        "text-embedding-v3",
        chat = false,
        embedding = true,
        vision = false,
        input = Some("0.07"),
        output = None,
        note = QWEN_INTL_EMBEDDING_NOTE
    ),
];

#[must_use]
pub fn provider_supports_capability(
    provider_kind: SupportedProviderKind,
    capability: &str,
) -> bool {
    match provider_kind {
        SupportedProviderKind::OpenAi => {
            matches!(capability, CAPABILITY_CHAT | CAPABILITY_EMBEDDINGS | CAPABILITY_VISION)
        }
        SupportedProviderKind::DeepSeek => capability == CAPABILITY_CHAT,
        SupportedProviderKind::Qwen => {
            matches!(capability, CAPABILITY_CHAT | CAPABILITY_EMBEDDINGS | CAPABILITY_VISION)
        }
    }
}

#[must_use]
pub fn provider_is_configured(settings: &Settings, provider_kind: SupportedProviderKind) -> bool {
    match provider_kind {
        SupportedProviderKind::OpenAi => has_secret(&settings.openai_api_key),
        SupportedProviderKind::DeepSeek => has_secret(&settings.deepseek_api_key),
        SupportedProviderKind::Qwen => has_secret(&settings.qwen_api_key),
    }
}

#[must_use]
pub fn supported_provider_catalog(
    settings: &Settings,
    defaults: &RuntimeProviderProfileDefaults,
) -> Vec<SupportedProviderCatalogEntry> {
    [SupportedProviderKind::OpenAi, SupportedProviderKind::DeepSeek, SupportedProviderKind::Qwen]
        .into_iter()
        .map(|provider_kind| {
            let default_models = BTreeMap::from_iter([
                (
                    ROLE_INDEXING.to_string(),
                    catalog_default_model(
                        &defaults.indexing,
                        provider_kind,
                        fallback_default_model(provider_kind, ROLE_INDEXING),
                    ),
                ),
                (
                    ROLE_EMBEDDING.to_string(),
                    catalog_default_model(
                        &defaults.embedding,
                        provider_kind,
                        fallback_default_model(provider_kind, ROLE_EMBEDDING),
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    catalog_default_model(
                        &defaults.answer,
                        provider_kind,
                        fallback_default_model(provider_kind, ROLE_ANSWER),
                    ),
                ),
                (
                    ROLE_VISION.to_string(),
                    catalog_default_model(
                        &defaults.vision,
                        provider_kind,
                        fallback_default_model(provider_kind, ROLE_VISION),
                    ),
                ),
            ]);

            let available_models = BTreeMap::from_iter(
                [ROLE_INDEXING, ROLE_EMBEDDING, ROLE_ANSWER, ROLE_VISION]
                    .into_iter()
                    .filter(|role| role_supported_by_provider(provider_kind, role))
                    .map(|role| {
                        let preferred = default_models.get(role).cloned().unwrap_or_else(|| {
                            fallback_default_model(provider_kind, role).to_string()
                        });
                        (
                            role.to_string(),
                            role_models(
                                &preferred,
                                provider_model_names_for_role(provider_kind, role),
                            ),
                        )
                    }),
            );

            SupportedProviderCatalogEntry {
                provider_kind,
                supported_capabilities: supported_capabilities_for_provider(provider_kind),
                default_models,
                available_models,
                is_configured: provider_is_configured(settings, provider_kind),
            }
        })
        .collect()
}

#[must_use]
pub fn built_in_pricing_catalog_seeds() -> Vec<CatalogPricingSeedDefinition> {
    let mut entries = Vec::new();
    for provider_kind in [
        SupportedProviderKind::OpenAi,
        SupportedProviderKind::DeepSeek,
        SupportedProviderKind::Qwen,
    ] {
        for definition in provider_model_definitions(provider_kind) {
            if definition.supports_chat {
                for capability in [
                    PRICING_CAPABILITY_INDEXING,
                    PRICING_CAPABILITY_GRAPH_EXTRACT,
                    PRICING_CAPABILITY_ANSWER,
                ] {
                    entries.push(CatalogPricingSeedDefinition {
                        provider_kind,
                        model_name: definition.model_name,
                        capability,
                        billing_unit: BILLING_UNIT_PER_1M_TOKENS,
                        input_price: definition.input_price,
                        output_price: definition.output_price,
                        note: definition.note,
                    });
                }
            }

            if definition.supports_vision {
                entries.push(CatalogPricingSeedDefinition {
                    provider_kind,
                    model_name: definition.model_name,
                    capability: PRICING_CAPABILITY_VISION,
                    billing_unit: BILLING_UNIT_PER_1M_TOKENS,
                    input_price: definition.input_price,
                    output_price: definition.output_price,
                    note: definition.note,
                });
            }

            if definition.supports_embedding {
                entries.push(CatalogPricingSeedDefinition {
                    provider_kind,
                    model_name: definition.model_name,
                    capability: PRICING_CAPABILITY_EMBEDDING,
                    billing_unit: BILLING_UNIT_PER_1M_INPUT_TOKENS,
                    input_price: definition.input_price,
                    output_price: None,
                    note: definition.note,
                });
            }
        }
    }
    entries
}

#[must_use]
pub fn available_models_for_role(
    settings: &Settings,
    defaults: &RuntimeProviderProfileDefaults,
    provider_kind: SupportedProviderKind,
    role: &str,
) -> Vec<String> {
    supported_provider_catalog(settings, defaults)
        .into_iter()
        .find(|entry| entry.provider_kind == provider_kind)
        .and_then(|entry| entry.available_models.get(role).cloned())
        .unwrap_or_default()
}

#[must_use]
pub fn model_is_available_for_role(
    settings: &Settings,
    defaults: &RuntimeProviderProfileDefaults,
    provider_kind: SupportedProviderKind,
    role: &str,
    model_name: &str,
) -> bool {
    available_models_for_role(settings, defaults, provider_kind, role)
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(model_name.trim()))
}

#[must_use]
pub fn pricing_requirement_for_role(role: &str) -> Option<(&'static str, &'static str)> {
    match role {
        ROLE_INDEXING => Some((PRICING_CAPABILITY_INDEXING, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_EMBEDDING => Some((PRICING_CAPABILITY_EMBEDDING, BILLING_UNIT_PER_1M_INPUT_TOKENS)),
        ROLE_ANSWER => Some((PRICING_CAPABILITY_ANSWER, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_VISION => Some((PRICING_CAPABILITY_VISION, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_QUERY_INTENT => Some((PRICING_CAPABILITY_QUERY_INTENT, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_RERANK => Some((PRICING_CAPABILITY_RERANK, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_GRAPH_SUMMARY => Some((PRICING_CAPABILITY_GRAPH_SUMMARY, BILLING_UNIT_PER_1M_TOKENS)),
        ROLE_EXTRACTION_RECOVERY => {
            Some((PRICING_CAPABILITY_EXTRACTION_RECOVERY, BILLING_UNIT_PER_1M_TOKENS))
        }
        _ => None,
    }
}

#[must_use]
pub fn pricing_target_for_selection(
    selection: &ProviderModelSelection,
    role: &str,
) -> Option<RuntimePricingTarget> {
    let (capability, billing_unit) = pricing_requirement_for_role(role)?;
    Some(RuntimePricingTarget {
        role: role.to_string(),
        provider_kind: selection.provider_kind,
        model_name: selection.model_name.clone(),
        capability: capability.to_string(),
        billing_unit: billing_unit.to_string(),
    })
}

#[must_use]
pub fn pricing_targets_for_profile(
    profile: &EffectiveProviderProfile,
) -> Vec<RuntimePricingTarget> {
    [
        pricing_target_for_selection(&profile.indexing, ROLE_INDEXING),
        pricing_target_for_selection(&profile.embedding, ROLE_EMBEDDING),
        pricing_target_for_selection(&profile.answer, ROLE_ANSWER),
        pricing_target_for_selection(&profile.vision, ROLE_VISION),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[must_use]
pub fn retrieval_intelligence_targets_for_profile(
    profile: &EffectiveProviderProfile,
) -> Vec<RuntimePricingTarget> {
    [
        pricing_target_for_selection(&profile.indexing, ROLE_QUERY_INTENT),
        pricing_target_for_selection(&profile.answer, ROLE_RERANK),
        pricing_target_for_selection(&profile.answer, ROLE_GRAPH_SUMMARY),
        pricing_target_for_selection(&profile.indexing, ROLE_EXTRACTION_RECOVERY),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn provider_model_definitions(
    provider_kind: SupportedProviderKind,
) -> &'static [ProviderModelDefinition] {
    match provider_kind {
        SupportedProviderKind::OpenAi => OPENAI_MODELS,
        SupportedProviderKind::DeepSeek => DEEPSEEK_MODELS,
        SupportedProviderKind::Qwen => QWEN_MODELS,
    }
}

fn supported_capabilities_for_provider(provider_kind: SupportedProviderKind) -> Vec<String> {
    match provider_kind {
        SupportedProviderKind::OpenAi | SupportedProviderKind::Qwen => {
            vec![CAPABILITY_CHAT.into(), CAPABILITY_EMBEDDINGS.into(), CAPABILITY_VISION.into()]
        }
        SupportedProviderKind::DeepSeek => vec![CAPABILITY_CHAT.into()],
    }
}

fn fallback_default_model(provider_kind: SupportedProviderKind, role: &str) -> &'static str {
    match (provider_kind, role) {
        (SupportedProviderKind::OpenAi, ROLE_INDEXING) => "gpt-5-mini",
        (SupportedProviderKind::OpenAi, ROLE_EMBEDDING) => "text-embedding-3-large",
        (SupportedProviderKind::OpenAi, ROLE_ANSWER) => "gpt-5.4",
        (SupportedProviderKind::OpenAi, ROLE_VISION) => "gpt-5-mini",
        (SupportedProviderKind::DeepSeek, ROLE_INDEXING) => "deepseek-chat",
        (SupportedProviderKind::DeepSeek, ROLE_ANSWER) => "deepseek-reasoner",
        (SupportedProviderKind::Qwen, ROLE_INDEXING) => "qwen-plus",
        (SupportedProviderKind::Qwen, ROLE_EMBEDDING) => "text-embedding-v4",
        (SupportedProviderKind::Qwen, ROLE_ANSWER) => "qwen3-max",
        (SupportedProviderKind::Qwen, ROLE_VISION) => "qwen3.5-plus",
        _ => "",
    }
}

fn role_supported_by_provider(provider_kind: SupportedProviderKind, role: &str) -> bool {
    match role {
        ROLE_INDEXING | ROLE_ANSWER => provider_supports_capability(provider_kind, CAPABILITY_CHAT),
        ROLE_EMBEDDING => provider_supports_capability(provider_kind, CAPABILITY_EMBEDDINGS),
        ROLE_VISION => provider_supports_capability(provider_kind, CAPABILITY_VISION),
        _ => false,
    }
}

fn provider_model_names_for_role(
    provider_kind: SupportedProviderKind,
    role: &str,
) -> Vec<&'static str> {
    provider_model_definitions(provider_kind)
        .iter()
        .filter(|definition| match role {
            ROLE_INDEXING | ROLE_ANSWER => definition.supports_chat,
            ROLE_EMBEDDING => definition.supports_embedding,
            ROLE_VISION => definition.supports_vision,
            _ => false,
        })
        .map(|definition| definition.model_name)
        .collect()
}

fn catalog_default_model(
    selection: &ProviderModelSelection,
    provider_kind: SupportedProviderKind,
    fallback: &str,
) -> String {
    if selection.provider_kind == provider_kind {
        return selection.model_name.clone();
    }
    fallback.to_string()
}

fn role_models<I>(preferred: &str, fallback_models: I) -> Vec<String>
where
    I: IntoIterator<Item = &'static str>,
{
    let mut models = Vec::new();
    let preferred = preferred.trim();
    if !preferred.is_empty() {
        models.push(preferred.to_string());
    }
    for model in fallback_models {
        if !models.iter().any(|candidate| candidate == model) {
            models.push(model.to_string());
        }
    }
    models
}

fn has_secret(value: &Option<String>) -> bool {
    value.as_deref().map(str::trim).is_some_and(|secret| !secret.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settings() -> Settings {
        Settings {
            bind_addr: "0.0.0.0:8080".into(),
            service_role: "all".into(),
            database_url: "postgres://postgres:postgres@127.0.0.1:5432/rustrag".into(),
            database_max_connections: 20,
            redis_url: "redis://127.0.0.1:6379".into(),
            arangodb_url: "http://127.0.0.1:8529".into(),
            arangodb_database: "rustrag".into(),
            arangodb_username: "root".into(),
            arangodb_password: "rustrag-dev".into(),
            arangodb_request_timeout_seconds: 15,
            arangodb_bootstrap_collections: true,
            arangodb_bootstrap_views: true,
            arangodb_bootstrap_graph: true,
            arangodb_bootstrap_vector_indexes: true,
            arangodb_vector_dimensions: 3072,
            arangodb_vector_index_n_lists: 100,
            arangodb_vector_index_default_n_probe: 8,
            arangodb_vector_index_training_iterations: 25,
            service_name: "rustrag-backend".into(),
            environment: "local".into(),
            log_filter: "info".into(),
            openai_api_key: Some("openai-key".into()),
            deepseek_api_key: None,
            qwen_api_key: Some("qwen-key".into()),
            qwen_api_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into(),
            bootstrap_token: None,
            bootstrap_claim_enabled: false,
            legacy_ui_bootstrap_enabled: false,
            legacy_bootstrap_token_endpoint_enabled: false,
            destructive_fresh_bootstrap_required: false,
            destructive_allow_legacy_startup_side_effects: false,
            frontend_origin: "http://127.0.0.1:19000".into(),
            ui_session_secret: "secret".into(),
            ui_default_locale: "ru".into(),
            ui_bootstrap_admin_login: None,
            ui_bootstrap_admin_email: None,
            ui_bootstrap_admin_name: None,
            ui_bootstrap_admin_password: None,
            ui_bootstrap_admin_api_token: None,
            ui_session_ttl_hours: 720,
            upload_max_size_mb: 50,
            ingestion_worker_concurrency: 4,
            ingestion_worker_lease_seconds: 300,
            ingestion_worker_heartbeat_interval_seconds: 15,
            llm_http_timeout_seconds: 120,
            llm_transport_retry_attempts: 3,
            llm_transport_retry_base_delay_ms: 250,
            runtime_default_indexing_provider: "openai".into(),
            runtime_default_indexing_model: "gpt-5.4-mini".into(),
            runtime_default_embedding_provider: "openai".into(),
            runtime_default_embedding_model: "text-embedding-3-large".into(),
            runtime_default_answer_provider: "openai".into(),
            runtime_default_answer_model: "gpt-5.4".into(),
            runtime_default_vision_provider: "openai".into(),
            runtime_default_vision_model: "gpt-5.4-mini".into(),
            runtime_live_validation_enabled: false,
            query_intent_cache_ttl_hours: 24,
            query_intent_cache_max_entries_per_library: 500,
            query_rerank_enabled: true,
            query_rerank_candidate_limit: 24,
            query_balanced_context_enabled: true,
            runtime_graph_extract_recovery_enabled: true,
            runtime_graph_extract_recovery_max_attempts: 2,
            runtime_graph_extract_resume_downgrade_level_one_after_replays: 3,
            runtime_graph_extract_resume_downgrade_level_two_after_replays: 5,
            runtime_graph_summary_refresh_batch_size: 64,
            runtime_graph_targeted_reconciliation_enabled: true,
            runtime_graph_targeted_reconciliation_max_targets: 128,
            runtime_document_activity_freshness_seconds: 45,
            runtime_document_stalled_after_seconds: 180,
            runtime_graph_filter_empty_relations: true,
            runtime_graph_filter_degenerate_self_loops: true,
            runtime_graph_convergence_warning_backlog_threshold: 1,
            mcp_memory_default_read_window_chars: 12_000,
            mcp_memory_max_read_window_chars: 50_000,
            mcp_memory_default_search_limit: 10,
            mcp_memory_max_search_limit: 25,
            mcp_memory_idempotency_retention_hours: 72,
            mcp_memory_audit_enabled: true,
            runtime_pricing_seed_from_env: true,
            runtime_pricing_default_currency: "USD".into(),
            openai_input_price_per_1m: 0.25,
            openai_output_price_per_1m: 2.0,
            deepseek_input_price_per_1m: 0.27,
            deepseek_output_price_per_1m: 1.10,
            qwen_input_price_per_1m: 0.07,
            qwen_chat_input_price_per_1m: 0.0,
            qwen_chat_output_price_per_1m: 0.0,
            qwen_vision_input_price_per_1m: 0.0,
            qwen_vision_output_price_per_1m: 0.0,
        }
    }

    #[test]
    fn catalog_marks_configuration_and_roles() {
        let settings = sample_settings();
        let defaults = RuntimeProviderProfileDefaults::from_settings(&settings);
        let catalog = supported_provider_catalog(&settings, &defaults);

        let openai = catalog
            .iter()
            .find(|entry| entry.provider_kind == SupportedProviderKind::OpenAi)
            .expect("openai entry");
        let deepseek = catalog
            .iter()
            .find(|entry| entry.provider_kind == SupportedProviderKind::DeepSeek)
            .expect("deepseek entry");
        let qwen = catalog
            .iter()
            .find(|entry| entry.provider_kind == SupportedProviderKind::Qwen)
            .expect("qwen entry");

        assert!(openai.is_configured);
        assert!(!deepseek.is_configured);
        assert!(qwen.is_configured);
        assert!(
            openai
                .available_models
                .get(ROLE_ANSWER)
                .expect("openai models")
                .contains(&"gpt-5.4-pro".to_string())
        );
        assert!(
            openai
                .available_models
                .get(ROLE_INDEXING)
                .expect("openai models")
                .contains(&"gpt-5-mini".to_string())
        );
        assert_eq!(
            deepseek.available_models.get(ROLE_ANSWER).expect("answer models")[0],
            "deepseek-reasoner",
        );
        assert!(
            qwen.available_models
                .get(ROLE_INDEXING)
                .expect("qwen indexing models")
                .contains(&"qwen3-max".to_string())
        );
        assert!(
            qwen.available_models
                .get(ROLE_INDEXING)
                .expect("qwen indexing models")
                .contains(&"qwen3-coder-plus".to_string())
        );
        assert!(
            qwen.available_models
                .get(ROLE_VISION)
                .expect("qwen vision models")
                .contains(&"qwen3-vl-plus".to_string())
        );
        assert_eq!(
            qwen.available_models.get(ROLE_EMBEDDING).expect("embedding models")[0],
            "text-embedding-v4",
        );
    }

    #[test]
    fn built_in_pricing_catalog_exposes_current_seed_entries() {
        let seeds = built_in_pricing_catalog_seeds();

        assert!(seeds.iter().any(|seed| {
            seed.provider_kind == SupportedProviderKind::OpenAi
                && seed.model_name == "gpt-5.4"
                && seed.capability == PRICING_CAPABILITY_ANSWER
                && seed.input_price == Some("2.50")
                && seed.output_price == Some("15.00")
        }));
        assert!(seeds.iter().any(|seed| {
            seed.provider_kind == SupportedProviderKind::Qwen
                && seed.model_name == "qwen3-max"
                && seed.capability == PRICING_CAPABILITY_ANSWER
                && seed.input_price == Some("1.2")
                && seed.output_price == Some("6")
        }));
        assert!(seeds.iter().any(|seed| {
            seed.provider_kind == SupportedProviderKind::Qwen
                && seed.model_name == "text-embedding-v4"
                && seed.capability == PRICING_CAPABILITY_EMBEDDING
                && seed.input_price == Some("0.07")
                && seed.output_price.is_none()
        }));
        assert!(seeds.iter().any(|seed| {
            seed.provider_kind == SupportedProviderKind::DeepSeek
                && seed.model_name == "deepseek-chat"
                && seed.capability == PRICING_CAPABILITY_INDEXING
                && seed.input_price == Some("0.28")
                && seed.output_price == Some("0.42")
        }));
    }
}
