import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { useApp } from '@/shared/contexts/app-context'
import type { DemoScenario, Role, Theme } from '@/shared/types'
import { PreferencesContext, type Preferences } from './preferences-context'

function readStored<T>(key: string, fallback: T): T {
  try {
    const value = localStorage.getItem(key)
    return value === null ? fallback : (JSON.parse(value) as T)
  } catch {
    return fallback
  }
}

function useStoredState<T>(key: string, fallback: T) {
  const [value, setValue] = useState<T>(() => readStored(key, fallback))
  const update = useCallback(
    (next: T) => {
      setValue(next)
      localStorage.setItem(key, JSON.stringify(next))
    },
    [key],
  )
  return [value, update] as const
}

/**
 * Starting width of the assistant's debug panel. Its bounds live beside it so
 * a stored value from an older, wider window cannot leave the panel occupying
 * most of the screen.
 */
const ASSISTANT_DEBUG_WIDTH_DEFAULT = 330
export const ASSISTANT_DEBUG_WIDTH_MIN = 260
export const ASSISTANT_DEBUG_WIDTH_MAX = 900

export function PreferencesProvider({ children }: Readonly<{ children: ReactNode }>) {
  const app = useApp()
  const [theme, setTheme] = useStoredState<Theme>('ironrag.v1.theme', 'system')
  const [sidebarCollapsed, setSidebarCollapsed] = useStoredState(
    'ironrag.v1.sidebar-collapsed',
    false,
  )
  const [sessionRailCollapsed, setSessionRailCollapsed] = useStoredState(
    'ironrag.v1.assistant.sessions-collapsed',
    false,
  )
  const [assistantDebugWidth, setAssistantDebugWidth] = useStoredState(
    'ironrag.v1.assistant.debug-width',
    ASSISTANT_DEBUG_WIDTH_DEFAULT,
  )
  const [devMode, setDevMode] = useStoredState('ironrag.v1.developer-mode', false)
  const [scenario, setScenario] = useState<DemoScenario>('ready')

  // Bridge: role and active scope come from the resolved session, not local
  // state. Fail-closed to viewer when unauthenticated.
  const role: Role = app.user?.role ?? 'viewer'
  const activeWorkspaceId = app.activeWorkspace?.id ?? ''
  const activeLibraryId = app.activeLibrary?.id ?? ''

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const apply = () => {
      const isDark = theme === 'dark' || (theme === 'system' && media.matches)
      document.documentElement.dataset.theme = isDark ? 'dark' : 'light'
      document.documentElement.style.colorScheme = isDark ? 'dark' : 'light'
    }
    apply()
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [theme])

  const value = useMemo<Preferences>(
    () => ({
      role,
      theme,
      setTheme,
      locale: app.locale,
      setLocale: app.setLocale,
      activeWorkspaceId,
      activeLibraryId,
      sidebarCollapsed,
      setSidebarCollapsed,
      sessionRailCollapsed,
      setSessionRailCollapsed,
      assistantDebugWidth,
      setAssistantDebugWidth,
      devMode,
      setDevMode,
      scenario,
      setScenario,
    }),
    [
      role,
      theme,
      setTheme,
      app.locale,
      app.setLocale,
      activeWorkspaceId,
      activeLibraryId,
      sidebarCollapsed,
      setSidebarCollapsed,
      sessionRailCollapsed,
      setSessionRailCollapsed,
      assistantDebugWidth,
      setAssistantDebugWidth,
      devMode,
      setDevMode,
      scenario,
    ],
  )

  return <PreferencesContext.Provider value={value}>{children}</PreferencesContext.Provider>
}
