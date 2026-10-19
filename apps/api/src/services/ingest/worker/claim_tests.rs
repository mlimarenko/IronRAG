use super::claim::memory_bound_process_claim_limit;

#[test]
fn memory_bound_process_claim_limit_serializes_tiny_workers() {
    assert_eq!(memory_bound_process_claim_limit(691), 1);
}

#[test]
fn memory_bound_process_claim_limit_scales_with_soft_limit() {
    assert_eq!(memory_bound_process_claim_limit(2304), 4);
}

#[test]
fn memory_bound_process_claim_limit_is_disabled_without_soft_limit() {
    assert_eq!(memory_bound_process_claim_limit(0), usize::MAX);
}
