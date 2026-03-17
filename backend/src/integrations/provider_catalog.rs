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

pub const CAPABILITY_CHAT: &str = "chat";
pub const CAPABILITY_EMBEDDINGS: &str = "embeddings";
pub const CAPABILITY_VISION: &str = "vision";
pub const PRICING_CAPABILITY_INDEXING: &str = "indexing";
pub const PRICING_CAPABILITY_EMBEDDING: &str = "embedding";
pub const PRICING_CAPABILITY_ANSWER: &str = "answer";
pub const PRICING_CAPABILITY_VISION: &str = "vision";
pub const BILLING_UNIT_PER_1M_INPUT_TOKENS: &str = "per_1m_input_tokens";
pub const BILLING_UNIT_PER_1M_TOKENS: &str = "per_1m_tokens";

pub const ROLE_INDEXING: &str = "indexing";
pub const ROLE_EMBEDDING: &str = "embedding";
pub const ROLE_ANSWER: &str = "answer";
pub const ROLE_VISION: &str = "vision";

const OPENAI_CHAT_MODELS: &[&str] = &["gpt-5-mini", "gpt-5.4"];
const OPENAI_EMBEDDING_MODELS: &[&str] = &["text-embedding-3-large", "text-embedding-3-small"];
const OPENAI_VISION_MODELS: &[&str] = &["gpt-5-mini", "gpt-5.4"];
const DEEPSEEK_CHAT_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
const DEEPSEEK_REASONING_MODELS: &[&str] = &["deepseek-reasoner", "deepseek-chat"];
const QWEN_INDEXING_MODELS: &[&str] = &["qwen-plus", "qwen-flash", "qwen-max"];
const QWEN_ANSWER_MODELS: &[&str] = &["qwen-max", "qwen-plus", "qwen-flash"];
const QWEN_EMBEDDING_MODELS: &[&str] = &["text-embedding-v4", "text-embedding-v3"];
const QWEN_VISION_MODELS: &[&str] = &["qwen-vl-max", "qwen-vl-plus"];

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
    vec![
        SupportedProviderCatalogEntry {
            provider_kind: SupportedProviderKind::OpenAi,
            supported_capabilities: vec![
                CAPABILITY_CHAT.into(),
                CAPABILITY_EMBEDDINGS.into(),
                CAPABILITY_VISION.into(),
            ],
            default_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    catalog_default_model(
                        &defaults.indexing,
                        SupportedProviderKind::OpenAi,
                        "gpt-5-mini",
                    ),
                ),
                (
                    ROLE_EMBEDDING.to_string(),
                    catalog_default_model(
                        &defaults.embedding,
                        SupportedProviderKind::OpenAi,
                        "text-embedding-3-large",
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    catalog_default_model(
                        &defaults.answer,
                        SupportedProviderKind::OpenAi,
                        "gpt-5.4",
                    ),
                ),
                (
                    ROLE_VISION.to_string(),
                    catalog_default_model(
                        &defaults.vision,
                        SupportedProviderKind::OpenAi,
                        "gpt-5-mini",
                    ),
                ),
            ]),
            available_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.indexing,
                            SupportedProviderKind::OpenAi,
                            "gpt-5-mini",
                        ),
                        OPENAI_CHAT_MODELS,
                    ),
                ),
                (
                    ROLE_EMBEDDING.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.embedding,
                            SupportedProviderKind::OpenAi,
                            "text-embedding-3-large",
                        ),
                        OPENAI_EMBEDDING_MODELS,
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.answer,
                            SupportedProviderKind::OpenAi,
                            "gpt-5.4",
                        ),
                        OPENAI_CHAT_MODELS,
                    ),
                ),
                (
                    ROLE_VISION.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.vision,
                            SupportedProviderKind::OpenAi,
                            "gpt-5-mini",
                        ),
                        OPENAI_VISION_MODELS,
                    ),
                ),
            ]),
            is_configured: provider_is_configured(settings, SupportedProviderKind::OpenAi),
        },
        SupportedProviderCatalogEntry {
            provider_kind: SupportedProviderKind::DeepSeek,
            supported_capabilities: vec![CAPABILITY_CHAT.into()],
            default_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    catalog_default_model(
                        &defaults.indexing,
                        SupportedProviderKind::DeepSeek,
                        "deepseek-chat",
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    catalog_default_model(
                        &defaults.answer,
                        SupportedProviderKind::DeepSeek,
                        "deepseek-reasoner",
                    ),
                ),
            ]),
            available_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.indexing,
                            SupportedProviderKind::DeepSeek,
                            "deepseek-chat",
                        ),
                        DEEPSEEK_CHAT_MODELS,
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.answer,
                            SupportedProviderKind::DeepSeek,
                            "deepseek-reasoner",
                        ),
                        DEEPSEEK_REASONING_MODELS,
                    ),
                ),
            ]),
            is_configured: provider_is_configured(settings, SupportedProviderKind::DeepSeek),
        },
        SupportedProviderCatalogEntry {
            provider_kind: SupportedProviderKind::Qwen,
            supported_capabilities: vec![
                CAPABILITY_CHAT.into(),
                CAPABILITY_EMBEDDINGS.into(),
                CAPABILITY_VISION.into(),
            ],
            default_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    catalog_default_model(
                        &defaults.indexing,
                        SupportedProviderKind::Qwen,
                        "qwen-plus",
                    ),
                ),
                (
                    ROLE_EMBEDDING.to_string(),
                    catalog_default_model(
                        &defaults.embedding,
                        SupportedProviderKind::Qwen,
                        "text-embedding-v4",
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    catalog_default_model(
                        &defaults.answer,
                        SupportedProviderKind::Qwen,
                        "qwen-max",
                    ),
                ),
                (
                    ROLE_VISION.to_string(),
                    catalog_default_model(
                        &defaults.vision,
                        SupportedProviderKind::Qwen,
                        "qwen-vl-max",
                    ),
                ),
            ]),
            available_models: BTreeMap::from([
                (
                    ROLE_INDEXING.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.indexing,
                            SupportedProviderKind::Qwen,
                            "qwen-plus",
                        ),
                        QWEN_INDEXING_MODELS,
                    ),
                ),
                (
                    ROLE_EMBEDDING.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.embedding,
                            SupportedProviderKind::Qwen,
                            "text-embedding-v4",
                        ),
                        QWEN_EMBEDDING_MODELS,
                    ),
                ),
                (
                    ROLE_ANSWER.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.answer,
                            SupportedProviderKind::Qwen,
                            "qwen-max",
                        ),
                        QWEN_ANSWER_MODELS,
                    ),
                ),
                (
                    ROLE_VISION.to_string(),
                    role_models(
                        &catalog_default_model(
                            &defaults.vision,
                            SupportedProviderKind::Qwen,
                            "qwen-vl-max",
                        ),
                        QWEN_VISION_MODELS,
                    ),
                ),
            ]),
            is_configured: provider_is_configured(settings, SupportedProviderKind::Qwen),
        },
    ]
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

fn role_models(preferred: &str, fallback_models: &[&str]) -> Vec<String> {
    let mut models = Vec::with_capacity(fallback_models.len() + 1);
    let preferred = preferred.trim();
    if !preferred.is_empty() {
        models.push(preferred.to_string());
    }
    for model in fallback_models {
        if !models.iter().any(|candidate| candidate == model) {
            models.push((*model).to_string());
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
            database_url: "postgres://postgres:postgres@127.0.0.1:5432/rustrag".into(),
            database_max_connections: 20,
            redis_url: "redis://127.0.0.1:6379".into(),
            neo4j_uri: "127.0.0.1:7687".into(),
            neo4j_username: "neo4j".into(),
            neo4j_password: "rustrag-dev".into(),
            neo4j_database: "neo4j".into(),
            neo4j_max_connections: 16,
            service_name: "rustrag-backend".into(),
            environment: "local".into(),
            log_filter: "info".into(),
            openai_api_key: Some("openai-key".into()),
            deepseek_api_key: None,
            qwen_api_key: Some("qwen-key".into()),
            qwen_api_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into(),
            bootstrap_token: None,
            frontend_origin: "http://127.0.0.1:19000".into(),
            ui_session_secret: "secret".into(),
            ui_default_locale: "ru".into(),
            ui_bootstrap_admin_login: None,
            ui_bootstrap_admin_email: None,
            ui_bootstrap_admin_name: None,
            ui_bootstrap_admin_password: None,
            ui_session_ttl_hours: 720,
            upload_max_size_mb: 50,
            ingestion_worker_concurrency: 4,
            ingestion_worker_lease_seconds: 300,
            ingestion_worker_heartbeat_interval_seconds: 15,
            llm_http_timeout_seconds: 120,
            runtime_default_indexing_provider: "openai".into(),
            runtime_default_indexing_model: "gpt-5-mini".into(),
            runtime_default_embedding_provider: "openai".into(),
            runtime_default_embedding_model: "text-embedding-3-large".into(),
            runtime_default_answer_provider: "openai".into(),
            runtime_default_answer_model: "gpt-5.4".into(),
            runtime_default_vision_provider: "openai".into(),
            runtime_default_vision_model: "gpt-5-mini".into(),
            runtime_live_validation_enabled: false,
            runtime_query_intent_cache_ttl_hours: 24,
            runtime_query_intent_cache_max_entries_per_library: 500,
            runtime_query_rerank_enabled: true,
            runtime_query_rerank_candidate_limit: 24,
            runtime_query_balanced_context_enabled: true,
            runtime_graph_extract_recovery_enabled: true,
            runtime_graph_extract_recovery_max_attempts: 2,
            runtime_graph_summary_refresh_batch_size: 64,
            runtime_graph_targeted_reconciliation_enabled: true,
            runtime_graph_targeted_reconciliation_max_targets: 128,
            runtime_document_activity_freshness_seconds: 45,
            runtime_document_stalled_after_seconds: 180,
            runtime_graph_filter_empty_relations: true,
            runtime_graph_filter_degenerate_self_loops: true,
            runtime_graph_convergence_warning_backlog_threshold: 1,
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
        assert_eq!(
            openai.available_models.get(ROLE_EMBEDDING).expect("embedding models")[0],
            "text-embedding-3-large",
        );
        assert_eq!(
            deepseek.available_models.get(ROLE_ANSWER).expect("answer models")[0],
            "deepseek-reasoner",
        );
        assert_eq!(
            qwen.available_models.get(ROLE_EMBEDDING).expect("embedding models")[0],
            "text-embedding-v4",
        );
        assert_eq!(
            qwen.available_models.get(ROLE_INDEXING).expect("indexing models")[0],
            "qwen-plus",
        );
        assert_eq!(qwen.available_models.get(ROLE_ANSWER).expect("answer models")[0], "qwen-max",);
        assert_eq!(
            qwen.available_models.get(ROLE_VISION).expect("vision models")[0],
            "qwen-vl-max",
        );
    }
}
