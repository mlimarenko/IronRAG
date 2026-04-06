use serde_json::json;

use crate::{
    agent_runtime::task::RuntimeTaskSpec,
    domains::agent_runtime::RuntimeOutputMode,
    integrations::llm::{
        ChatRequest, ChatRequestSeed, build_structured_chat_request, build_text_chat_request,
    },
};

#[must_use]
pub fn build_provider_request(
    spec: &RuntimeTaskSpec,
    seed: ChatRequestSeed,
    prompt: String,
) -> ChatRequest {
    match spec.output_mode {
        RuntimeOutputMode::Text => build_text_chat_request(seed, prompt),
        RuntimeOutputMode::Structured => {
            build_structured_chat_request(seed, prompt, json!({ "type": "json_object" }))
        }
    }
}
