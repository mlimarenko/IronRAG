import { createContext, useContext } from 'react'
import type { DemoScenario, Locale, Role, Theme } from '@/shared/types'

// Local, device-scoped UI preferences (theme, panel collapse state, developer
// mode). Session-derived values (role, locale, active workspace/library) are
// bridged from AppContext so the real signed-in identity — never a demo
// switcher — drives what the shell renders.
export type Preferences = {
  role: Role
  theme: Theme
  setTheme: (theme: Theme) => void
  locale: Locale
  setLocale: (locale: Locale) => void
  activeWorkspaceId: string
  activeLibraryId: string
  sidebarCollapsed: boolean
  setSidebarCollapsed: (value: boolean) => void
  sessionRailCollapsed: boolean
  setSessionRailCollapsed: (value: boolean) => void
  /** Width of the assistant's debug panel, in pixels. */
  assistantDebugWidth: number
  setAssistantDebugWidth: (value: number) => void
  devMode: boolean
  setDevMode: (value: boolean) => void
  scenario: DemoScenario
  setScenario: (scenario: DemoScenario) => void
}

export const PreferencesContext = createContext<Preferences | null>(null)

export function usePreferences() {
  const context = useContext(PreferencesContext)
  if (!context) throw new Error('usePreferences must be used inside PreferencesProvider')
  return context
}
