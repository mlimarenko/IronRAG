use crate::domains::{query_modes::RuntimeQueryMode, retrieval::ChatPromptState};

const DEFAULT_SYSTEM_PROMPT: &str = "Answer using knowledge from the active library only. Gather all required context from that knowledge base before answering. Find the documents needed to support the answer, and when fragment-level evidence is insufficient, request or use broader full-document context. If the active library does not contain enough grounded evidence, say that plainly instead of guessing.";
const PLACEHOLDER_TITLE: &str = "New chat";
const MAX_DERIVED_TITLE_CHARS: usize = 72;

#[derive(Debug, Default, Clone, Copy)]
pub struct ChatSessionsService;

impl ChatSessionsService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn default_system_prompt(self) -> String {
        DEFAULT_SYSTEM_PROMPT.to_string()
    }

    #[must_use]
    pub fn normalize_system_prompt(self, prompt: &str) -> String {
        let normalized = prompt
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n")
            .split('\n')
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        if normalized.is_empty() { DEFAULT_SYSTEM_PROMPT.to_string() } else { normalized }
    }

    #[must_use]
    pub fn restore_default_prompt(self) -> String {
        self.default_system_prompt()
    }

    #[must_use]
    pub fn derive_prompt_state(self, prompt: &str) -> ChatPromptState {
        if self.normalize_system_prompt(prompt) == DEFAULT_SYSTEM_PROMPT {
            ChatPromptState::Default
        } else {
            ChatPromptState::Customized
        }
    }

    #[must_use]
    pub const fn recommended_mode(self) -> RuntimeQueryMode {
        RuntimeQueryMode::Hybrid
    }

    #[must_use]
    pub const fn placeholder_title(self) -> &'static str {
        PLACEHOLDER_TITLE
    }

    #[must_use]
    pub fn is_placeholder_title(self, title: &str) -> bool {
        title.trim().eq_ignore_ascii_case(PLACEHOLDER_TITLE)
    }

    #[must_use]
    pub fn derive_title_from_question(self, question: &str) -> String {
        let normalized = question
            .split("\n\nQuestion:")
            .last()
            .unwrap_or(question)
            .trim()
            .trim_start_matches("Question:")
            .trim()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        if normalized.is_empty() {
            return PLACEHOLDER_TITLE.to_string();
        }

        let truncated = normalized.chars().take(MAX_DERIVED_TITLE_CHARS).collect::<String>();
        if normalized.chars().count() > MAX_DERIVED_TITLE_CHARS {
            format!("{}...", truncated.trim_end())
        } else {
            truncated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ChatSessionsService;
    use crate::domains::{query_modes::RuntimeQueryMode, retrieval::ChatPromptState};

    #[test]
    fn derives_default_prompt_state_from_normalized_prompt() {
        let service = ChatSessionsService::new();
        assert_eq!(
            service.derive_prompt_state(&format!("  {}  ", service.default_system_prompt())),
            ChatPromptState::Default
        );
    }

    #[test]
    fn recommends_hybrid_mode() {
        let service = ChatSessionsService::new();
        assert_eq!(service.recommended_mode(), RuntimeQueryMode::Hybrid);
    }
}
