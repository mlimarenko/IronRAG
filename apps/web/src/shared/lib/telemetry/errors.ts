import { redactText } from './redaction'

type BrowserErrorSource = 'window.error' | 'unhandledrejection'

export type BrowserErrorDetails = {
  source: BrowserErrorSource
  type: string
  message: string
  stack: string
}

export function describeError(error: unknown): { type: string; message: string; stack: string } {
  if (error instanceof Error) {
    return {
      type: error.name || 'Error',
      message: redactText(error.message),
      stack: redactText(error.stack ?? '', 2048),
    }
  }
  if (typeof error === 'string') {
    return { type: 'Error', message: redactText(error), stack: '' }
  }
  return { type: 'UnknownError', message: redactText(String(error)), stack: '' }
}

/**
 * Capture uncaught exceptions and unhandled promise rejections. Resource errors
 * (failed images or scripts) are ignored: they have no exception and belong to
 * network diagnostics, not to the JS error stream.
 */
export function installGlobalErrorHandlers(
  onError: (details: BrowserErrorDetails) => void,
): () => void {
  const onWindowError = (event: ErrorEvent) => {
    if (!event.error) return
    const described = describeError(event.error)
    onError({ source: 'window.error', ...described })
  }
  const onUnhandledRejection = (event: PromiseRejectionEvent) => {
    const described = describeError(event.reason)
    onError({ source: 'unhandledrejection', ...described })
  }
  window.addEventListener('error', onWindowError)
  window.addEventListener('unhandledrejection', onUnhandledRejection)
  return () => {
    window.removeEventListener('error', onWindowError)
    window.removeEventListener('unhandledrejection', onUnhandledRejection)
  }
}
