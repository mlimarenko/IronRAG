import { createContext, useContext } from 'react';

/** Theme preference. `system` follows the OS `prefers-color-scheme`. */
export type ThemePreference = 'light' | 'dark' | 'system';

/** The actually-applied theme after resolving `system`. */
export type ResolvedTheme = 'light' | 'dark';

export interface PreferencesContextValue {
  /** The user's chosen theme preference (may be `system`). */
  theme: ThemePreference;
  /** The concrete theme currently applied to the document. */
  resolvedTheme: ResolvedTheme;
  setTheme: (theme: ThemePreference) => void;
  /** Cycle light → dark → system → light, for a single toggle control. */
  cycleTheme: () => void;
  /**
   * Developer mode — a remembered per-user switch that later unlocks debug /
   * advanced surfaces (assistant debug inspector, AI catalog, raw payloads).
   * Persisted to localStorage so it survives reloads.
   */
  developerMode: boolean;
  setDeveloperMode: (enabled: boolean) => void;
  toggleDeveloperMode: () => void;
}

export const PreferencesContext = createContext<PreferencesContextValue | null>(null);

/**
 * Access theme + developer-mode preferences. Must be used within
 * `PreferencesProvider` (mounted at the app root). Debug/dev surfaces gate
 * their visibility off `developerMode`; chrome reads `resolvedTheme`.
 */
export function usePreferences(): PreferencesContextValue {
  const ctx = useContext(PreferencesContext);
  if (!ctx) throw new Error('usePreferences must be used within PreferencesProvider');
  return ctx;
}

/**
 * Non-throwing read of developer mode. Returns `false` when no
 * `PreferencesProvider` is mounted (e.g. isolated component tests) instead of
 * throwing, so feature surfaces can gate debug/advanced affordances behind it
 * without forcing every test harness to wrap the provider. In production the
 * provider is always mounted at the app root, so this reflects the real flag.
 */
export function useDeveloperMode(): boolean {
  const ctx = useContext(PreferencesContext);
  return ctx?.developerMode ?? false;
}
