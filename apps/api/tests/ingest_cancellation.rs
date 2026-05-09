use std::{error::Error, time::Duration};

use ironrag_backend::services::ingest::cancellation::{
    StageError, StageResult, ensure_not_cancelled,
};
use tokio_util::sync::CancellationToken;

async fn fake_long_running_stage(cancellation_token: CancellationToken) -> StageResult<()> {
    loop {
        ensure_not_cancelled(&cancellation_token)?;
        tokio::select! {
            () = cancellation_token.cancelled() => return Err(StageError::Cancelled),
            () = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
    }
}

#[tokio::test]
async fn fake_long_running_stage_returns_cancelled_quickly()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let cancellation_token = CancellationToken::new();
    let task_token = cancellation_token.clone();
    let stage = tokio::spawn(async move { fake_long_running_stage(task_token).await });

    tokio::time::sleep(Duration::from_millis(10)).await;
    let started_at = std::time::Instant::now();
    cancellation_token.cancel();

    let stage_result = tokio::time::timeout(Duration::from_millis(200), stage).await??;

    assert_eq!(stage_result, Err(StageError::Cancelled));
    assert!(started_at.elapsed() < Duration::from_millis(200));
    Ok(())
}
