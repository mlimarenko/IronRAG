export type OperatorViewState = 'loading' | 'empty' | 'success' | 'degraded' | 'error' | 'blocked'

export interface OperatorContext {
  workspaceLabel?: string
  projectLabel?: string
  detail?: string
}

export interface OperatorStateSummary {
  state: OperatorViewState
  title: string
  message: string
  context?: OperatorContext
}
