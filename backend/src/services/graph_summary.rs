#[derive(Debug, Clone, Default)]
pub struct GraphSummaryService;

impl GraphSummaryService {
    #[must_use]
    pub fn should_batch_refresh(&self, affected_targets: usize, batch_limit: usize) -> bool {
        affected_targets > 0 && affected_targets <= batch_limit
    }
}
