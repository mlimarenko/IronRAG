use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum StageError {
    #[error("ingest stage canceled")]
    Cancelled,
}

pub type StageResult<T> = Result<T, StageError>;

pub fn ensure_not_cancelled(cancellation_token: &CancellationToken) -> StageResult<()> {
    if cancellation_token.is_cancelled() { Err(StageError::Cancelled) } else { Ok(()) }
}

pub fn anyhow_is_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<StageError>().is_some()
}
