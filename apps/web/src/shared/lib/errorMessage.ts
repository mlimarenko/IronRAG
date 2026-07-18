/**
 * Canonical "extract a human message from an unknown thrown value" helper.
 * Pages and adapters share the same fallback so error toasts always look the
 * same regardless of which surface raised them.
 */
export function errorMessage(err: unknown, fallback: string): string {
  if (typeof err === 'string' && err.trim()) return err
  if (err instanceof Error) return err.message
  if (typeof err === 'object' && err !== null && 'message' in err) {
    const msg = (err as { message?: unknown }).message
    if (typeof msg === 'string') return msg
  }
  return fallback
}

/**
 * Canonical message for in-page error boundaries. Backend/provider errors can
 * contain operator-only diagnostics, so default page surfaces show the stable
 * user-facing copy chosen by the caller.
 */
export function safeErrorMessage(_err: unknown, fallback: string): string {
  return fallback
}
