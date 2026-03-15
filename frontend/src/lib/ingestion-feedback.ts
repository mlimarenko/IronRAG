import { isUnauthorizedApiError } from 'src/boot/api'

export interface FeedbackState {
  tone: 'success' | 'warning' | 'danger' | 'info'
  title: string
  body: string
  detail?: string
}

export function createErrorFeedback(
  error: unknown,
  fallbackBody: string,
  describeError: (message: string) => { title: string; body: string; detail?: string },
): FeedbackState {
  const message = error instanceof Error ? error.message : fallbackBody
  const copy = describeError(message)
  return {
    tone: 'danger',
    title: copy.title,
    body: copy.body,
    detail: copy.detail,
  }
}

export function isCreateActionUnauthorized(error: unknown): boolean {
  return isUnauthorizedApiError(error)
}
