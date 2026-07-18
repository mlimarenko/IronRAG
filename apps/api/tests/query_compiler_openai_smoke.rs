//! Real-OpenAI smoke test for the `QueryCompiler` stage.
//!
//! This is the validation advisor flagged as mandatory before downstream
//! consumer migration: the hand-written JSON Schema in
//! `domains/query_ir.rs::query_ir_json_schema` must survive OpenAI's
//! `strict: true` structured outputs mode, and a real `gpt-5.4-nano` call
//! must deserialise back into a `QueryIR`.
//!
//! Ignored by default. Run explicitly when validating the schema:
//! ```text
//! IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=<base64-json-map> cargo test -p ironrag-backend \
//!   --test query_compiler_openai_smoke -- --ignored --nocapture
//! ```

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ironrag_backend::domains::ai::AiBindingPurpose;
use ironrag_backend::integrations::llm::UnifiedGateway;
use ironrag_backend::services::ai_catalog_service::ResolvedRuntimeBinding;
use ironrag_backend::services::query::compiler::{CompileHistoryTurn, QueryCompilerService};
use std::env;
use uuid::Uuid;
use zeroize::Zeroize as _;

struct SmokeCase {
    question: &'static str,
    history: &'static [(&'static str, &'static str)],
}

fn cases() -> Vec<SmokeCase> {
    vec![
        SmokeCase { question: "How do I configure the payment module?", history: &[] },
        SmokeCase {
            question: "What endpoints does /health expose and what does it return?",
            history: &[],
        },
        SmokeCase {
            question: "Compare REST and SOAP transport protocols in this corpus.",
            history: &[],
        },
        SmokeCase {
            question: "How do I configure it?",
            history: &[
                ("user", "do we have a payment module?"),
                ("assistant", "Yes, the payment module is documented in the library."),
            ],
        },
        SmokeCase { question: "What documents are in this library?", history: &[] },
    ]
}

fn binding_from_env() -> Option<ResolvedRuntimeBinding> {
    let mut encoded_provider_keys = env::var("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64").ok()?;
    if encoded_provider_keys.is_empty()
        || encoded_provider_keys != encoded_provider_keys.trim()
        || encoded_provider_keys.len() > 1_048_576_usize.div_ceil(3) * 4
    {
        encoded_provider_keys.zeroize();
        return None;
    }
    let mut decoded_provider_keys = match STANDARD.decode(&encoded_provider_keys) {
        Ok(decoded) if decoded.len() <= 1_048_576 => decoded,
        Ok(mut decoded) => {
            decoded.zeroize();
            encoded_provider_keys.zeroize();
            return None;
        }
        Err(_) => {
            encoded_provider_keys.zeroize();
            return None;
        }
    };
    let mut canonical_encoding = STANDARD.encode(&decoded_provider_keys);
    let is_canonical = canonical_encoding == encoded_provider_keys;
    canonical_encoding.zeroize();
    encoded_provider_keys.zeroize();
    if !is_canonical {
        decoded_provider_keys.zeroize();
        return None;
    }
    let parsed = serde_json::from_slice::<std::collections::BTreeMap<String, String>>(
        &decoded_provider_keys,
    );
    decoded_provider_keys.zeroize();
    let mut provider_keys = parsed.ok()?;
    let api_key = provider_keys.remove("openai")?;
    for unused_api_key in provider_keys.values_mut() {
        unused_api_key.zeroize();
    }
    Some(ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose: AiBindingPurpose::QueryCompile,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "openai".to_string(),
        provider_base_url: None,
        provider_api_style: "openai".to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some(api_key),
        model_catalog_id: Uuid::now_v7(),
        model_name: env::var("IRONRAG_QUERY_COMPILE_MODEL")
            .unwrap_or_else(|_| "gpt-5.4-nano".to_string()),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    })
}

fn settings_stub() -> anyhow::Result<ironrag_backend::app::config::Settings> {
    // Minimum viable Settings for UnifiedGateway::from_settings. Only
    // the HTTP timeout knob is read.
    let mut settings = ironrag_backend::app::config::Settings::from_env()?;
    settings.llm_http_timeout_seconds = 60;
    Ok(settings)
}

#[tokio::test]
#[ignore]
async fn openai_strict_schema_round_trip() -> anyhow::Result<()> {
    let Some(binding) = binding_from_env() else {
        tracing::info!("skipping: provider kind openai is absent from the provider API-key map");
        return Ok(());
    };
    let settings = settings_stub()?;
    let gateway = UnifiedGateway::from_settings(&settings)?;
    let service = QueryCompilerService;

    let mut failures = Vec::<(String, String)>::new();
    for case in cases() {
        let history: Vec<CompileHistoryTurn> = case
            .history
            .iter()
            .map(|(role, content)| CompileHistoryTurn {
                role: (*role).to_string(),
                content: (*content).to_string(),
            })
            .collect();

        let outcome =
            service.compile_with_gateway(&gateway, &binding, case.question, &history).await;

        match outcome {
            Ok(outcome) => {
                tracing::info!(
                    question = case.question,
                    act = ?outcome.ir.act,
                    scope = ?outcome.ir.scope,
                    language = ?outcome.ir.language,
                    target_types = ?outcome.ir.target_types,
                    confidence = outcome.ir.confidence,
                    "query compiler smoke case completed",
                );
            }
            Err(error) => {
                failures.push((case.question.to_string(), format!("{error:#}")));
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "{} / 5 smoke cases failed:\n{}",
            failures.len(),
            failures
                .iter()
                .map(|(q, reason)| format!("  `{q}` — {reason}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}
