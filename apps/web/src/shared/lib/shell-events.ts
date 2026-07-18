/**
 * Tiny window-event bridge for cross-surface shell intents.
 *
 * Some affordances live in a feature (e.g. the dashboard empty state) but the
 * thing they need to drive — opening the library scope picker, opening the
 * shell's "create library" dialog, or opening the global command palette — is
 * owned by the AppShell. Rather than thread setState callbacks through context
 * (which would couple every feature to the shell's internal dialog state), the
 * feature dispatches a typed intent on `window` and the shell listens for it.
 *
 * This keeps the AppShell edit surgical (a couple of listeners) and lets any
 * authenticated surface request a shell action without a prop drill.
 */

export type ShellIntent =
  /** Open the library scope selector dropdown in the shell. */
  | 'open-library-picker'
  /** Open the shell's "create library" dialog (operator+). */
  | 'create-library'
  /** Open (toggle) the global command palette. */
  | 'open-command-palette'

const EVENT_NAME = 'ironrag:shell-intent'

/** Dispatch a shell intent. No-ops in non-DOM environments (SSR/tests). */
export function emitShellIntent(intent: ShellIntent): void {
  if (typeof window === 'undefined') return
  window.dispatchEvent(new CustomEvent<ShellIntent>(EVENT_NAME, { detail: intent }))
}

/**
 * Subscribe to a specific shell intent. Returns an unsubscribe function so
 * callers can clean up in a `useEffect` teardown.
 */
export function onShellIntent(intent: ShellIntent, handler: () => void): () => void {
  if (typeof window === 'undefined') return () => {}
  const listener = (event: Event) => {
    if ((event as CustomEvent<ShellIntent>).detail === intent) handler()
  }
  window.addEventListener(EVENT_NAME, listener)
  return () => window.removeEventListener(EVENT_NAME, listener)
}
