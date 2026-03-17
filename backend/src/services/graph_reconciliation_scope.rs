#[derive(Debug, Clone, Default)]
pub struct GraphReconciliationScopeService;

impl GraphReconciliationScopeService {
    #[must_use]
    pub fn prefer_targeted_reconciliation(
        &self,
        affected_node_count: usize,
        affected_relationship_count: usize,
        enabled: bool,
        max_targets: usize,
    ) -> bool {
        enabled
            && affected_node_count + affected_relationship_count > 0
            && affected_node_count + affected_relationship_count <= max_targets
    }
}
