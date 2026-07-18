import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'

import {
  PreferencesContext,
  type PreferencesContextValue,
  type ResolvedTheme,
  type ThemePreference,
} from './preferences-context'

const THEME_STORAGE_KEY = 'ironrag_theme'
const DEVELOPER_MODE_STORAGE_KEY = 'ironrag_developer_mode'

function isThemePreference(value: string | null): value is ThemePreference {
  return value === 'light' || value === 'dark' || value === 'system'
}

function readStoredTheme(): ThemePreference {
  if (typeof window === 'undefined') return 'system'
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY)
  return isThemePreference(stored) ? stored : 'system'
}

function readStoredDeveloperMode(): boolean {
  if (typeof window === 'undefined') return false
  return window.localStorage.getItem(DEVELOPER_MODE_STORAGE_KEY) === 'true'
}

function systemPrefersDark(): boolean {
  if (typeof window === 'undefined' || !window.matchMedia) return false
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

function resolveTheme(theme: ThemePreference, prefersDark: boolean): ResolvedTheme {
  if (theme === 'system') return prefersDark ? 'dark' : 'light'
  return theme
}

/**
 * Applies the resolved theme to the document root by toggling the `.dark`
 * class that `index.css` keys every dark-mode token off, and mirrors it to
 * the native `color-scheme` so form controls/scrollbars match.
 */
function applyThemeToDocument(resolved: ResolvedTheme) {
  if (typeof document === 'undefined') return
  const root = document.documentElement
  root.classList.toggle('dark', resolved === 'dark')
  root.style.colorScheme = resolved
}

function nextThemePreference(current: ThemePreference): ThemePreference {
  switch (current) {
    case 'light':
      return 'dark'
    case 'dark':
      return 'system'
    case 'system':
      return 'light'
  }
}

/**
 * Provides theme + developer-mode preferences to the tree. Both persist to
 * localStorage per browser; theme additionally tracks the OS preference when
 * set to `system`.
 */
export function PreferencesProvider({ children }: Readonly<{ children: ReactNode }>) {
  const [theme, setTheme] = useState<ThemePreference>(readStoredTheme)
  const [prefersDark, setPrefersDark] = useState<boolean>(systemPrefersDark)
  const [developerMode, setDeveloperMode] = useState<boolean>(readStoredDeveloperMode)

  // Track the OS color-scheme so `system` stays live.
  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = (event: MediaQueryListEvent) => setPrefersDark(event.matches)
    media.addEventListener('change', onChange)
    return () => media.removeEventListener('change', onChange)
  }, [])

  const resolvedTheme = resolveTheme(theme, prefersDark)

  // Reflect the resolved theme onto the document whenever it changes.
  useEffect(() => {
    applyThemeToDocument(resolvedTheme)
  }, [resolvedTheme])

  const persistTheme = useCallback((next: ThemePreference) => {
    setTheme(next)
    window.localStorage.setItem(THEME_STORAGE_KEY, next)
  }, [])

  const cycleTheme = useCallback(() => {
    setTheme((current) => {
      const next = nextThemePreference(current)
      window.localStorage.setItem(THEME_STORAGE_KEY, next)
      return next
    })
  }, [])

  const persistDeveloperMode = useCallback((enabled: boolean) => {
    setDeveloperMode(enabled)
    window.localStorage.setItem(DEVELOPER_MODE_STORAGE_KEY, String(enabled))
  }, [])

  const toggleDeveloperMode = useCallback(() => {
    setDeveloperMode((current) => {
      const next = !current
      window.localStorage.setItem(DEVELOPER_MODE_STORAGE_KEY, String(next))
      return next
    })
  }, [])

  const value = useMemo<PreferencesContextValue>(
    () => ({
      theme,
      resolvedTheme,
      setTheme: persistTheme,
      cycleTheme,
      developerMode,
      setDeveloperMode: persistDeveloperMode,
      toggleDeveloperMode,
    }),
    [
      theme,
      resolvedTheme,
      persistTheme,
      cycleTheme,
      developerMode,
      persistDeveloperMode,
      toggleDeveloperMode,
    ],
  )

  return <PreferencesContext.Provider value={value}>{children}</PreferencesContext.Provider>
}
