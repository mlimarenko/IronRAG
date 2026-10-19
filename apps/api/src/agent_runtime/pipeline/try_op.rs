use std::future::Future;

/// # Errors
/// Returns the error produced by the provided asynchronous runtime pipeline operation.
pub async fn run_async_try_op<TInput, TOutput, TError, TFuture, TOp>(
    input: TInput,
    op: TOp,
) -> Result<TOutput, TError>
where
    TOp: FnOnce(TInput) -> TFuture,
    TFuture: Future<Output = Result<TOutput, TError>>,
{
    op(input).await
}
