import { createContext, useContext } from 'react'

/// The workspace/library picker lives in the top bar, which every screen that
/// reports "no library selected" could point at but none could open. Those
/// screens were dead ends: a first-time reader was told to pick a library with
/// nothing on the screen that picks one. Exposing the shell's own switcher
/// keeps a single picker rather than growing a second one per screen.
export type ScopeSwitcherControl = {
  openScopeSwitcher: () => void
}

export const ScopeSwitcherContext = createContext<ScopeSwitcherControl | null>(null)

/// Returns `null` outside the application shell (the login route, tests that
/// render a screen on its own) so callers can fall back to text-only guidance
/// instead of crashing.
export function useScopeSwitcher(): ScopeSwitcherControl | null {
  return useContext(ScopeSwitcherContext)
}
