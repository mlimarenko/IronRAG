use std::env;

pub fn openai_api_key() -> Option<String> {
    env::var("RUSTRAG_OPENAI_API_KEY").ok().filter(|value| !value.trim().is_empty())
}

pub fn deepseek_api_key() -> Option<String> {
    env::var("RUSTRAG_DEEPSEEK_API_KEY").ok().filter(|value| !value.trim().is_empty())
}

pub fn qwen_api_key() -> Option<String> {
    env::var("RUSTRAG_QWEN_API_KEY").ok().filter(|value| !value.trim().is_empty())
}

pub fn require_openai_api_key() -> Result<String, String> {
    openai_api_key().ok_or_else(|| "RUSTRAG_OPENAI_API_KEY is not set".to_string())
}

pub fn require_deepseek_api_key() -> Result<String, String> {
    deepseek_api_key().ok_or_else(|| "RUSTRAG_DEEPSEEK_API_KEY is not set".to_string())
}

pub fn require_qwen_api_key() -> Result<String, String> {
    qwen_api_key().ok_or_else(|| "RUSTRAG_QWEN_API_KEY is not set".to_string())
}
