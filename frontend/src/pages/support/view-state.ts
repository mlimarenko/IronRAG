import type { OperatorStateSummary, OperatorViewState } from 'src/contracts/operator-state'

export function buildOperatorState(
  state: OperatorViewState,
  title: string,
  message: string,
  context?: OperatorStateSummary['context'],
): OperatorStateSummary {
  return {
    state,
    title,
    message,
    context,
  }
}
