#[derive(Debug, Clone, Default)]
pub struct ExtractionRecoveryService;

impl ExtractionRecoveryService {
    #[must_use]
    pub fn should_attempt_parser_repair(&self, raw_output: &str, enabled: bool) -> bool {
        enabled
            && raw_output.contains('{')
            && raw_output.contains('}')
            && !raw_output.trim().is_empty()
    }

    #[must_use]
    pub fn should_attempt_second_pass(
        &self,
        entity_count: usize,
        relationship_count: usize,
        enabled: bool,
        max_attempts: usize,
    ) -> bool {
        enabled && max_attempts > 1 && entity_count + relationship_count <= 1
    }
}
