pub trait RuntimeOp<TInput> {
    type Output;

    fn run(&self, input: TInput) -> Self::Output;
}

impl<TInput, TOutput, TFunc> RuntimeOp<TInput> for TFunc
where
    TFunc: Fn(TInput) -> TOutput,
{
    type Output = TOutput;

    fn run(&self, input: TInput) -> Self::Output {
        self(input)
    }
}
