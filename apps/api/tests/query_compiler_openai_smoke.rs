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
//! IRONRAG_OPENAI_API_KEY=sk-... cargo test -p ironrag-backend \
//!   --test query_compiler_openai_smoke -- --ignored --nocapture
//! ```

use ironrag_backend::domains::ai::AiBindingPurpose;
use ironrag_backend::integrations::llm::UnifiedGateway;
use ironrag_backend::services::ai_catalog_service::ResolvedRuntimeBinding;
use ironrag_backend::services::query::compiler::{CompileHistoryTurn, QueryCompilerService};
use std::env;
use uuid::Uuid;

struct SmokeCase {
    question: &'static str,
    history: &'static [(&'static str, &'static str)],
}

fn cases() -> Vec<SmokeCase> {
    vec![
        SmokeCase { question: "как настроить сбп?", history: &[] },
        SmokeCase {
            question: "What endpoints does /health expose and what does it return?",
            history: &[],
        },
        SmokeCase {
            question: "Compare REST and SOAP transport protocols in this corpus.",
            history: &[],
        },
        SmokeCase {
            question: "а как настроить?",
            history: &[("user", "у нас есть СБП Сбербанк?"), ("assistant", "Да, модуль Sbersbp.")],
        },
        SmokeCase {
            question: "Какие документы есть в этой библиотеке?", history: &[]
        },
    ]
}

fn binding_from_env() -> Option<ResolvedRuntimeBinding> {
    let api_key = env::var("IRONRAG_OPENAI_API_KEY").ok()?;
    Some(ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose: AiBindingPurpose::QueryCompile,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "openai".to_string(),
        provider_base_url: None,
        provider_api_style: "openai".to_string(),
        credential_id: Uuid::now_v7(),
        api_key: Some(api_key),
        model_catalog_id: Uuid::now_v7(),
        model_name: env::var("IRONRAG_QUERY_COMPILE_MODEL")
            .unwrap_or_else(|_| "gpt-5.4-nano".to_string()),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    })
}

fn settings_stub() -> ironrag_backend::app::config::Settings {
    // Minimum viable Settings for UnifiedGateway::from_settings. Only
    // the transport_retry / timeout knobs are actually read.
    let mut settings = ironrag_backend::app::config::Settings::from_env()
        .expect("settings should load for smoke test");
    settings.llm_http_timeout_seconds = 60;
    settings.llm_transport_retry_attempts = 2;
    settings.llm_transport_retry_base_delay_ms = 250;
    settings
}

#[tokio::test]
#[ignore]
async fn openai_strict_schema_round_trip() {
    let Some(binding) = binding_from_env() else {
        eprintln!("skipping: IRONRAG_OPENAI_API_KEY is not set");
        return;
    };
    let settings = settings_stub();
    let gateway = UnifiedGateway::from_settings(&settings);
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
                println!(
                    "Q: {}\n  act={:?} scope={:?} lang={:?} targets={:?} conf={:.2} fallback={:?}",
                    case.question,
                    outcome.ir.act,
                    outcome.ir.scope,
                    outcome.ir.language,
                    outcome.ir.target_types,
                    outcome.ir.confidence,
                    outcome.fallback_reason,
                );
                if let Some(reason) = outcome.fallback_reason {
                    failures.push((case.question.to_string(), format!("fallback: {reason}")));
                }
            }
            Err(error) => {
                failures.push((case.question.to_string(), format!("{error:#}")));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} / 5 smoke cases failed:\n{}",
            failures.len(),
            failures
                .iter()
                .map(|(q, reason)| format!("  `{q}` — {reason}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
