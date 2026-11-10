mod chat;
mod embeddings;
mod gateway;
mod gateway_impl;
mod openai_compatible;
mod provider;
mod request;
mod response;
mod streaming;
mod types;

pub(crate) use embeddings::embedding_request_parameters;
pub use gateway::UnifiedGateway;
pub(crate) use provider::{disable_bounded_answer_reasoning, provider_auth_header_for_binding};
pub(crate) use request::provider_system_prompt;
pub use types::{
    ChatCompletionStatus, ChatMessage, ChatRequest, ChatRequestSeed, ChatResponse, ChatToolCall,
    ChatToolDef, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, LlmGateway, ToolUseRequest, ToolUseResponse, VisionRequest, VisionResponse,
    build_structured_chat_request, build_text_chat_request,
};

#[cfg(test)]
mod chat_tests;
#[cfg(test)]
mod embeddings_tests;
#[cfg(test)]
mod openai_compatible_tests;
#[cfg(test)]
mod provider_policy_tests;
#[cfg(test)]
mod provider_tests;
#[cfg(test)]
mod response_tests;
#[cfg(test)]
mod streaming_tests;
#[cfg(test)]
mod target_preparation_tests;
#[cfg(test)]
mod types_tests;
