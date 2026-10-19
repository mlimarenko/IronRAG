import { createContext, useContext } from 'react'

import type { BootstrapSetup } from '@/shared/api/auth'
import type { Library, Locale, User, Workspace } from '@/shared/types'

interface AppState {
  user: User | null
  workspaces: Workspace[]
  activeWorkspace: Workspace | null
  libraries: Library[]
  activeLibrary: Library | null
  locale: Locale
  isAuthenticated: boolean
  isBootstrapMode: boolean
  isBootstrapRequired: boolean
  /** Whether this deployment has a complete server-side SSO configuration. */
  ssoAvailable: boolean
  isLoading: boolean
  sessionError: string | null
  /** The server stopped accepting the session while the app was open. */
  isSessionExpired: boolean
}

export interface AppContextValue extends AppState {
  setUser: (user: User | null) => void
  setWorkspaces: (ws: Workspace[] | ((prev: Workspace[]) => Workspace[])) => void
  setActiveWorkspace: (ws: Workspace | null) => void
  setLibraries: (libs: Library[] | ((prev: Library[]) => Library[])) => void
  setActiveLibrary: (lib: Library | null) => void
  setLocale: (l: Locale) => void
  setIsBootstrapMode: (b: boolean) => void
  setIsBootstrapRequired: (b: boolean) => void
  /**
   * Land on a library named only by id, moving to the workspace that owns it.
   *
   * Takes no workspace: a library names its own, and a caller holding a bare id
   * (a link, a stored preference) has no authority to say which one it is.
   * Returns false only when that workspace is not one this viewer can see.
   */
  selectLibrary: (libraryId: string) => boolean
  login: (login: string, password: string) => Promise<void>
  logout: () => Promise<void>
  bootstrapSetup: (data: BootstrapSetup) => Promise<void>
  refreshSession: () => Promise<void>
}

export const AppContext = createContext<AppContextValue | null>(null)

export function useApp() {
  const ctx = useContext(AppContext)
  if (!ctx) throw new Error('useApp must be used within AppProvider')
  return ctx
}
