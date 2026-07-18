import { useState, useCallback, useEffect, useMemo, type ReactNode } from 'react'
import { authApi, ApiError } from '@/shared/api'
import i18n from '@/shared/i18n'
import type { BootstrapSetup, SessionResolveResponse } from '@/shared/api/auth'
import {
  AVAILABLE_LOCALES,
  type User,
  type Workspace,
  type Library,
  type Locale,
} from '@/shared/types'
import type { AiBindingPurpose } from '@/shared/api/generated'
import { AppContext, type AppContextValue } from './app-context'

function isLocale(value: string | null | undefined): value is Locale {
  return AVAILABLE_LOCALES.some((locale) => locale.code === value)
}

function preferredLocale(sessionLocale: string): Locale {
  const savedLocale = localStorage.getItem('ironrag_locale')
  if (isLocale(savedLocale)) return savedLocale
  return isLocale(sessionLocale) ? sessionLocale : AVAILABLE_LOCALES[0].code
}

function localizedShellName(slug: string, fallbackName: string, locale: Locale): string {
  if (slug === 'default') {
    return i18n.t('shell.defaultWorkspaceLabel', { lng: locale })
  }
  if (slug === 'default-library') {
    return i18n.t('shell.defaultLibraryLabel', { lng: locale })
  }
  return fallbackName
}

const QUERY_READY_PURPOSES = new Set<AiBindingPurpose>([
  'embed_chunk',
  'query_compile',
  'query_answer',
  'agent',
])

function libraryQueryReady(
  ingestionReady: boolean,
  queryReady: boolean | null | undefined,
  missingBindingPurposes: AiBindingPurpose[],
): boolean {
  return (
    queryReady ??
    (ingestionReady && !missingBindingPurposes.some((purpose) => QUERY_READY_PURPOSES.has(purpose)))
  )
}

function mapSessionToState(session: SessionResolveResponse, locale: Locale) {
  let user: User | null = null
  if (session.me) {
    const viewer = session.shellBootstrap?.viewer
    user = {
      id: viewer?.principalId ?? session.me.principal.id,
      login: viewer?.login ?? session.me.user?.login ?? session.me.principal.displayLabel,
      displayName:
        viewer?.displayName ?? session.me.user?.displayName ?? session.me.principal.displayLabel,
      accessLabel: viewer?.accessLabel ?? session.me.principal.displayLabel,
      role: viewer?.role ?? 'viewer',
    }
  }

  const workspaces: Workspace[] = (session.shellBootstrap?.workspaces ?? []).map((ws) => ({
    id: ws.id,
    name: localizedShellName(ws.slug, ws.name, locale),
    createdAt: '',
  }))

  const libraries: Library[] = (session.shellBootstrap?.libraries ?? []).map((lib) => {
    const missingBindingPurposes = lib.missingBindingPurposes
    return {
      id: lib.id,
      workspaceId: lib.workspaceId,
      name: localizedShellName(lib.slug, lib.name, locale),
      createdAt: '',
      // `ShellBootstrap.libraries` (LibrarySummary) does not carry the MCP
      // document-hint flag — only the full catalog library response does.
      // Bootstrap libraries always default to the hint enabled.
      includeDocumentHintInMcpAnswers: true,
      ingestionReady: lib.ingestionReady,
      queryReady: libraryQueryReady(lib.ingestionReady, lib.queryReady, missingBindingPurposes),
      missingBindingPurposes,
    }
  })

  const isBootstrapRequired =
    session.mode === 'bootstrap_required' || (session.bootstrapStatus?.setupRequired ?? false)

  return { user, workspaces, libraries, isBootstrapRequired, locale }
}

function resolveWorkspaceSelection(
  workspaces: Workspace[],
  savedWorkspaceId: string | null,
): Workspace | null {
  if (workspaces.length === 0) return null
  const savedWorkspace = savedWorkspaceId
    ? workspaces.find((workspace) => workspace.id === savedWorkspaceId)
    : null
  return savedWorkspace ?? workspaces[0] ?? null
}

function resolveLibrarySelection(
  libraries: Library[],
  activeWorkspaceId: string | null,
  savedLibraryId: string | null,
): Library | null {
  if (!activeWorkspaceId) return null
  const scopedLibraries = libraries.filter((library) => library.workspaceId === activeWorkspaceId)
  if (scopedLibraries.length === 0) return null
  const savedLibrary = savedLibraryId
    ? scopedLibraries.find((library) => library.id === savedLibraryId)
    : null
  return savedLibrary ?? scopedLibraries[0] ?? null
}

export function AppProvider({ children }: Readonly<{ children: ReactNode }>) {
  const [user, setUser] = useState<User | null>(null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [activeWorkspace, setActiveWorkspace] = useState<Workspace | null>(null)
  const [libraries, setLibraries] = useState<Library[]>([])
  const [activeLibrary, setActiveLibrary] = useState<Library | null>(null)
  const [locale, setLocale] = useState<Locale>('en')
  const persistedSetLocale = useCallback((l: Locale) => {
    setLocale(l)
    localStorage.setItem('ironrag_locale', l)
    // A translation update is best-effort: retain the selected locale and avoid
    // an unhandled rejection if the i18n backend is temporarily unavailable.
    i18n.changeLanguage(l).catch(() => undefined)
  }, [])
  const [isBootstrapMode, setIsBootstrapMode] = useState(false)
  const [isBootstrapRequired, setIsBootstrapRequired] = useState(false)
  const [isLoading, setIsLoading] = useState(true)
  const [sessionError, setSessionError] = useState<string | null>(null)

  const applySession = useCallback(
    (session: SessionResolveResponse) => {
      const resolvedLocale = preferredLocale(session.locale || 'en')
      const state = mapSessionToState(session, resolvedLocale)
      setUser(state.user)
      setWorkspaces(state.workspaces)
      setLibraries(state.libraries)
      setIsBootstrapRequired(state.isBootstrapRequired)
      persistedSetLocale(state.locale)

      const savedWsId = localStorage.getItem('ironrag_active_workspace')
      const savedLibId = localStorage.getItem('ironrag_active_library')
      const nextWorkspace = resolveWorkspaceSelection(state.workspaces, savedWsId)
      const nextLibrary = resolveLibrarySelection(
        state.libraries,
        nextWorkspace?.id ?? null,
        savedLibId,
      )

      setActiveWorkspace(nextWorkspace)
      setActiveLibrary(nextLibrary)

      if (nextWorkspace) localStorage.setItem('ironrag_active_workspace', nextWorkspace.id)
      else localStorage.removeItem('ironrag_active_workspace')

      if (nextLibrary) localStorage.setItem('ironrag_active_library', nextLibrary.id)
      else localStorage.removeItem('ironrag_active_library')
    },
    [persistedSetLocale],
  )

  // Resolve session on mount. Bootstrap of the AppContext provider runs
  // before the QueryClientProvider's tree exists for downstream consumers,
  // so this single one-shot fetch stays on the imperative auth API facade
  // intentionally. All other server-state reads flow through useQuery.
  useEffect(() => {
    let cancelled = false
    const resolveInitialSession = async () => {
      try {
        // eslint-disable-next-line no-restricted-syntax -- AppContext bootstrap, see comment above
        const session = await authApi.resolveSession()
        if (!cancelled) {
          applySession(session)
          setSessionError(null)
        }
      } catch (err) {
        if (!cancelled) {
          if (err instanceof ApiError && err.status === 401) {
            // Not authenticated — expected on first visit
            setUser(null)
          } else {
            setSessionError(err instanceof Error ? err.message : 'Session resolve failed')
          }
        }
      } finally {
        if (!cancelled) setIsLoading(false)
      }
    }
    resolveInitialSession().catch((error: unknown) => {
      if (!cancelled) {
        setSessionError(error instanceof Error ? error.message : 'Session resolve failed')
        setIsLoading(false)
      }
    })
    return () => {
      cancelled = true
    }
  }, [applySession])

  const login = useCallback(
    async (loginVal: string, password: string) => {
      await authApi.login(loginVal, password)
      const session = await authApi.resolveSession()
      applySession(session)
    },
    [applySession],
  )

  const logout = useCallback(async () => {
    try {
      await authApi.logout()
    } catch {
      // Ignore logout errors — clear local state regardless
    }
    setUser(null)
    setWorkspaces([])
    setLibraries([])
    setActiveWorkspace(null)
    setActiveLibrary(null)
    setIsBootstrapRequired(false)
  }, [])

  const bootstrapSetup = useCallback(
    async (data: BootstrapSetup) => {
      await authApi.bootstrapSetup(data)
      const session = await authApi.resolveSession()
      applySession(session)
      setIsBootstrapRequired(false)
    },
    [applySession],
  )

  const refreshSession = useCallback(async () => {
    const session = await authApi.resolveSession()
    applySession(session)
  }, [applySession])

  const filteredLibraries = useMemo(
    () => libraries.filter((l) => l.workspaceId === activeWorkspace?.id),
    [libraries, activeWorkspace?.id],
  )

  const persistedSetActiveWorkspace = useCallback(
    (ws: Workspace | null) => {
      setActiveWorkspace(ws)
      if (ws) localStorage.setItem('ironrag_active_workspace', ws.id)
      else localStorage.removeItem('ironrag_active_workspace')
      setActiveLibrary((prev) => {
        const nextLibrary =
          prev?.workspaceId === ws?.id
            ? prev
            : (libraries.find((library) => library.workspaceId === ws?.id) ?? null)
        if (nextLibrary) localStorage.setItem('ironrag_active_library', nextLibrary.id)
        else localStorage.removeItem('ironrag_active_library')
        return nextLibrary
      })
    },
    [libraries],
  )

  const persistedSetActiveLibrary = useCallback((lib: Library | null) => {
    setActiveLibrary(lib)
    if (lib) localStorage.setItem('ironrag_active_library', lib.id)
    else localStorage.removeItem('ironrag_active_library')
  }, [])

  const selectWorkspaceLibrary = useCallback(
    (workspaceId: string, libraryId: string): boolean => {
      const nextWorkspace = workspaces.find((workspace) => workspace.id === workspaceId) ?? null
      const nextLibrary =
        libraries.find(
          (library) => library.workspaceId === workspaceId && library.id === libraryId,
        ) ?? null
      if (!nextWorkspace || !nextLibrary) return false

      setActiveWorkspace(nextWorkspace)
      setActiveLibrary(nextLibrary)
      localStorage.setItem('ironrag_active_workspace', nextWorkspace.id)
      localStorage.setItem('ironrag_active_library', nextLibrary.id)
      return true
    },
    [libraries, workspaces],
  )

  // Memoize the context value so the ~18 app-wide consumers only re-render when
  // a slice they actually read changes. Without this, every provider render
  // (session resolve, selection change, locale switch) minted a fresh value
  // object and re-rendered the whole tree under the provider. The setters are
  // all stable (useState dispatch + useCallback), so they are safe deps.
  const value = useMemo<AppContextValue>(
    () => ({
      user,
      workspaces,
      activeWorkspace,
      libraries: filteredLibraries,
      activeLibrary,
      locale,
      isAuthenticated: !!user,
      isBootstrapMode,
      isBootstrapRequired,
      isLoading,
      sessionError,
      setUser,
      setWorkspaces,
      setActiveWorkspace: persistedSetActiveWorkspace,
      setLibraries,
      setActiveLibrary: persistedSetActiveLibrary,
      setLocale: persistedSetLocale,
      setIsBootstrapMode,
      setIsBootstrapRequired,
      selectWorkspaceLibrary,
      login,
      logout,
      bootstrapSetup,
      refreshSession,
    }),
    [
      user,
      workspaces,
      activeWorkspace,
      filteredLibraries,
      activeLibrary,
      locale,
      isBootstrapMode,
      isBootstrapRequired,
      isLoading,
      sessionError,
      persistedSetActiveWorkspace,
      persistedSetActiveLibrary,
      persistedSetLocale,
      selectWorkspaceLibrary,
      login,
      logout,
      bootstrapSetup,
      refreshSession,
    ],
  )

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>
}
