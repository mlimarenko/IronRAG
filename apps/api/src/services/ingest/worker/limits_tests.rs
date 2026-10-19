use std::{sync::Arc, time::Duration};

use tokio::sync::Semaphore;

use super::limits::{
    HeavyPipelineParallelismSetting, HeavyRevisionPipelinePermitTimeout,
    acquire_heavy_revision_pipeline_slot_within,
    auto_heavy_revision_pipeline_parallelism_for_limits, heavy_pipeline_parallelism_setting,
};

fn mib(value: u64) -> u64 {
    value * 1024 * 1024
}

#[test]
fn heavy_pipeline_parallelism_setting_preserves_auto_explicit_and_fail_safe_values() {
    for raw in [None, Some(""), Some("   "), Some("auto"), Some(" AuTo ")] {
        assert_eq!(heavy_pipeline_parallelism_setting(raw), HeavyPipelineParallelismSetting::Auto);
    }
    for (raw, value) in [("1", 1), (" 12 ", 12), ("+2", 2)] {
        assert_eq!(
            heavy_pipeline_parallelism_setting(Some(raw)),
            HeavyPipelineParallelismSetting::Configured(value)
        );
    }
    for raw in ["0", "-1", "invalid", "99999999999999999999999999999999999999999"] {
        assert_eq!(
            heavy_pipeline_parallelism_setting(Some(raw)),
            HeavyPipelineParallelismSetting::Invalid(raw)
        );
    }
    assert_eq!(
        heavy_pipeline_parallelism_setting(Some(" invalid ")),
        HeavyPipelineParallelismSetting::Invalid("invalid")
    );
}

#[tokio::test]
async fn saturated_heavy_revision_pipeline_returns_a_typed_timeout() {
    let semaphore = Arc::new(Semaphore::new(1));
    let _held = Arc::clone(&semaphore).acquire_owned().await.expect("first permit");

    // RED before the bounded acquire: the wait ignored the bound and stayed
    // pending, so this outer bound elapsed. GREEN after: a typed refusal.
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        acquire_heavy_revision_pipeline_slot_within(&semaphore, Duration::from_millis(20)),
    )
    .await
    .expect("bounded wait must return instead of hanging")
    .expect_err("a saturated heavy revision pipeline must refuse");

    let timeout = error
        .downcast_ref::<HeavyRevisionPipelinePermitTimeout>()
        .expect("the refusal must be the typed heavy-revision timeout");
    assert_eq!(timeout.timeout_ms, 20);
}

#[test]
fn auto_heavy_pipeline_parallelism_uses_cpu_memory_and_default_cap() {
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(6, Some(mib(8192)), 2), 4);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(2, Some(mib(8192)), 4), 2);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(6144)), 4), 3);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(4096)), 4), 1);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(8192)), 1), 2);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, None, 4), 1);
    assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(0, Some(mib(8192)), 4), 1);
}
