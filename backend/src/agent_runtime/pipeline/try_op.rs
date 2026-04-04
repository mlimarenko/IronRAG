use std::future::Future;

pub trait RuntimeTryOp<TInput> {
    type Output;
    type Error;

    /// # Errors
    /// Returns the operation-specific error for the provided input.
    fn run(&self, input: TInput) -> Result<Self::Output, Self::Error>;
}

impl<TInput, TOutput, TError, TFunc> RuntimeTryOp<TInput> for TFunc
where
    TFunc: Fn(TInput) -> Result<TOutput, TError>,
{
    type Output = TOutput;
    type Error = TError;

    fn run(&self, input: TInput) -> Result<Self::Output, Self::Error> {
        self(input)
    }
}

/// # Errors
/// Returns the error produced by the provided runtime pipeline operation.
#[allow(clippy::needless_pass_by_value)]
pub fn run_try_op<TInput, TOp>(input: TInput, op: TOp) -> Result<TOp::Output, TOp::Error>
where
    TOp: RuntimeTryOp<TInput>,
{
    op.run(input)
}

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
