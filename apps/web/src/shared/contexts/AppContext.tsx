import { useQueryClient } from '@tanstack/react-query'
import { useState, useCallback, useEffect, useMemo, useRef, type ReactNode } from 'react'
import { authApi, ApiError, Catalog } from '@/shared/api'
import { unwrap } from '@/shared/api/runtime'
import {
  markSessionSignedIn,
  markSessionSignedOut,
  subscribeToSessionExpired,
} from '@/shared/api/sessionLifecycle'
import i18n from '@/shared/i18n'
import type { BootstrapSetup, SessionResolveResponse } from '@/shared/api/auth'
import {
  AVAILABLE_LOCALES,
  type User,
  type Workspace,
  type Library,
  type Locale,
} from '@/shared/types'
import { mapCatalogLibrary, mapShellLibrary } from '@/shared/lib/library'
import {
  readScopePreference,
  writeScopePreference,
  type ScopePreference,
} from '@/shared/lib/scopePreference'
import { AppContext, type AppContextValue } from './app-context'

function isLocale(value: string | null | undefined): value is Locale {
  return AVAILABLE_LOCALES.some((locale) => locale.code === value)
}

function preferredLocale(sessionLocale: string): Locale {
  const savedLocale = localStorage.getItem('ironrag.v1.locale')
  if (isLocale(savedLocale)) return savedLocale
  return isLocale(sessionLocale) ? sessionLocale : AVAILABLE_LOCALES[0].code
}

function principalIdOf(session: SessionResolveResponse): string | null {
  return session.shellBootstrap?.viewer?.principalId ?? session.me?.principal.id ?? null
}

function toSelection(scope: ScopePreference) {
  return {
    ...(scope.workspaceId ? { activeWorkspaceId: scope.workspaceId } : {}),
    ...(scope.libraryId ? { activeLibraryId: scope.libraryId } : {}),
  }
}

/**
 * Can the stored library be selected from what this response already carries?
 *
 * The shell bootstrap ships a bounded sample of the active workspace's
 * libraries, so a library the user picked through the full-catalog search may
 * simply not be in it. When that happens the answer cannot honour the
 * preference and a second, pinned resolve is needed.
 */
function bootstrapCanHonourScope(session: SessionResolveResponse, scope: ScopePreference): boolean {
  if (!scope.workspaceId && !scope.libraryId) return true
  const bootstrap = session.shellBootstrap
  if (!bootstrap) return true
  // Libraries are only ever loaded for the workspace the server considers
  // active, so a stored workspace the server did not pick makes this answer
  // unusable no matter what it contains.
  if (scope.workspaceId && bootstrap.activeWorkspaceId !== scope.workspaceId) return false
  if (!scope.libraryId) return true
  return (bootstrap.libraries ?? []).some((library) => library.id === scope.libraryId)
}

/**
 * Resolve the session and land on the signed-in principal's own scope.
 *
 * The first call deliberately sends no scope hint: on a cold load nobody knows
 * yet which principal the cookie belongs to, and sending the last hint the
 * browser happens to hold is how one user inherits another's library. Once the
 * response names the principal, their stored preference is read and — only if
 * this answer cannot satisfy it — asked for once more with the library pinned.
 */
async function resolveSessionForStoredScope(): Promise<SessionResolveResponse> {
  const session = await authApi.resolveSession()
  const principalId = principalIdOf(session)
  if (!principalId) return session
  const scope = readScopePreference(principalId)
  if (bootstrapCanHonourScope(session, scope)) return session
  return authApi.resolveSession(toSelection(scope))
}

function localizedWorkspaceName(slug: string, fallbackName: string, locale: Locale): string {
  if (slug === 'default') {
    return i18n.t('shell.defaultWorkspaceLabel', { lng: locale })
  }
  return fallbackName
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
    name: localizedWorkspaceName(ws.slug, ws.name, locale),
    createdAt: '',
  }))

  const libraries: Library[] = (session.shellBootstrap?.libraries ?? []).map((library) =>
    mapShellLibrary(library, locale),
  )

  const isBootstrapRequired =
    session.mode === 'bootstrap_required' || (session.bootstrapStatus?.setupRequired ?? false)

  return {
    user,
    workspaces,
    libraries,
    isBootstrapRequired,
    ssoAvailable: session.bootstrapStatus.ssoAvailable,
    locale,
  }
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

interface LibrarySelection {
  library: Library | null
  /** A remembered id the bootstrap sample cannot answer for; resolve it server-side. */
  unresolvedId: string | null
}

function resolveLibrarySelection(
  libraries: Library[],
  activeWorkspaceId: string | null,
  savedLibraryId: string | null,
): LibrarySelection {
  if (!activeWorkspaceId) return { library: null, unresolvedId: null }
  const scopedLibraries = libraries.filter((library) => library.workspaceId === activeWorkspaceId)
  if (savedLibraryId) {
    const savedLibrary = scopedLibraries.find((library) => library.id === savedLibraryId)
    if (savedLibrary) return { library: savedLibrary, unresolvedId: null }
    // Present in the sample but under a different workspace: the sample already
    // answers this one, so there is nothing to ask the server about.
    if (libraries.some((library) => library.id === savedLibraryId)) {
      return { library: scopedLibraries[0] ?? null, unresolvedId: null }
    }
    // The bootstrap carries a bounded sample, so "absent from the sample" is
    // not "does not exist" — on a workspace with thousands of libraries the
    // remembered one is almost never in it. Falling back to the first library
    // here, and persisting that below, is what silently discarded the
    // selection on every switch and every reload.
    return { library: null, unresolvedId: savedLibraryId }
  }
  return { library: scopedLibraries[0] ?? null, unresolvedId: null }
}

export function AppProvider({ children }: Readonly<{ children: ReactNode }>) {
  const [user, setUser] = useState<User | null>(null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [activeWorkspace, setActiveWorkspace] = useState<Workspace | null>(null)
  const [libraries, setLibraries] = useState<Library[]>([])
  const [activeLibrary, setActiveLibrary] = useState<Library | null>(null)
  const [pendingLibraryId, setPendingLibraryId] = useState<string | null>(null)
  const [locale, setLocale] = useState<Locale>('en')
  useEffect(() => {
    document.documentElement.lang = locale
  }, [locale])
  const persistedSetLocale = useCallback((l: Locale) => {
    setLocale(l)
    localStorage.setItem('ironrag.v1.locale', l)
    // A translation update is best-effort: retain the selected locale and avoid
    // an unhandled rejection if the i18n backend is temporarily unavailable.
    i18n.changeLanguage(l).catch(() => undefined)
  }, [])
  const [isBootstrapMode, setIsBootstrapMode] = useState(false)
  const [isBootstrapRequired, setIsBootstrapRequired] = useState(false)
  const [ssoAvailable, setSsoAvailable] = useState(false)
  const [isLoading, setIsLoading] = useState(true)
  const [sessionError, setSessionError] = useState<string | null>(null)
  const [isSessionExpired, setIsSessionExpired] = useState(false)

  const queryClient = useQueryClient()
  const cachedIdentity = useRef<string | null>(null)

  /**
   * Cross the identity boundary: cached server state belongs to exactly one
   * principal (sign-in, sign-out, account switch), so a principal that leaves
   * takes its cache with it.
   *
   * The boundary has to be crossed in the same synchronous turn that changes
   * the identity, before React renders anything for the new one. Noticing the
   * change afterwards — from an effect on a component wrapping the routes —
   * put the wipe *after* the child effects of the very commit that mounts the
   * landing page, so that page's just-started fetch was cancelled silently.
   * React Query leaves a silently cancelled query frozen at `pending`/
   * `fetching` and notifies no observer, so the screen kept its skeleton with
   * no request ever reaching the network.
   */
  const enterIdentity = useCallback(
    (identity: string | null) => {
      const previous = cachedIdentity.current
      cachedIdentity.current = identity
      if (previous !== identity) queryClient.clear()
    },
    [queryClient],
  )

  const applySession = useCallback(
    (session: SessionResolveResponse) => {
      const resolvedLocale = preferredLocale(session.locale || 'en')
      const state = mapSessionToState(session, resolvedLocale)
      enterIdentity(state.user?.id ?? null)
      setUser(state.user)
      setWorkspaces(state.workspaces)
      setLibraries(state.libraries)
      setIsBootstrapRequired(state.isBootstrapRequired)
      setSsoAvailable(state.ssoAvailable)
      persistedSetLocale(state.locale)

      // The signed-in person's own remembered scope outranks the server's
      // "first library" default. Both resolvers validate the id against what
      // actually came back, so a deleted or no-longer-visible library falls
      // through to a sensible choice instead of leaving the shell stuck.
      const principalId = state.user?.id ?? null
      const scope = principalId ? readScopePreference(principalId) : {}
      const nextWorkspace = resolveWorkspaceSelection(
        state.workspaces,
        scope.workspaceId ?? session.shellBootstrap?.activeWorkspaceId ?? null,
      )
      const { library: nextLibrary, unresolvedId } = resolveLibrarySelection(
        state.libraries,
        nextWorkspace?.id ?? null,
        scope.libraryId ?? session.shellBootstrap?.activeLibraryId ?? null,
      )

      setActiveWorkspace(nextWorkspace)
      setActiveLibrary(nextLibrary)
      setPendingLibraryId(unresolvedId)

      // Keep the remembered id while it is still being resolved: the resolved
      // library if there is one, otherwise the id that has not resolved yet.
      const rememberedLibraryId = nextLibrary?.id ?? unresolvedId
      if (principalId) {
        writeScopePreference(principalId, {
          ...(nextWorkspace ? { workspaceId: nextWorkspace.id } : {}),
          // Keep the remembered id while it is still being resolved. Writing a
          // fallback over it would destroy the selection permanently, so the
          // person could never return to that library even after a reload.
          ...(rememberedLibraryId ? { libraryId: rememberedLibraryId } : {}),
        })
        setIsSessionExpired(false)
        markSessionSignedIn()
      }
    },
    [enterIdentity, persistedSetLocale],
  )

  const principalId = user?.id ?? null
  const persistScope = useCallback(
    (scope: ScopePreference) => {
      if (principalId) writeScopePreference(principalId, scope)
    },
    [principalId],
  )

  const refreshSession = useCallback(async () => {
    const session = await resolveSessionForStoredScope()
    applySession(session)
  }, [applySession])

  /**
   * Land on a library — and, with it, on the workspace that owns it.
   *
   * Selecting a library is ONE transition, not two. A library names its own
   * workspace, so the workspace never has to be supplied alongside it, and every
   * caller that used to write both was a second writer of the same transition —
   * the shape that lost the workspace switch in the scope dialog.
   *
   * `activeWorkspace` is read here only to decide whether the loaded library
   * sample has to be reloaded. It never decides what gets written: the stored
   * pair always comes from the library itself, so it cannot contradict itself
   * even when this runs in the same event as another scope update.
   */
  const applyLibrarySelection = useCallback(
    (library: Library): boolean => {
      const owner = workspaces.find((workspace) => workspace.id === library.workspaceId) ?? null
      // Workspaces arrive unsampled (apps/api/src/interfaces/shell.rs lists every
      // discoverable one), so a library whose workspace is not among them is not
      // a library this shell can coherently show.
      if (!owner) return false

      setActiveLibrary(library)
      setPendingLibraryId(null)
      if (owner.id !== activeWorkspace?.id) {
        setActiveWorkspace(owner)
        // The loaded libraries are a sample of the workspace being LEFT, so
        // every library picker would be empty until the shell is asked again.
        refreshSession().catch((error: unknown) => {
          setSessionError(error instanceof Error ? error.message : 'Session resolve failed')
        })
      }
      persistScope({ workspaceId: owner.id, libraryId: library.id })
      return true
    },
    [activeWorkspace?.id, persistScope, refreshSession, workspaces],
  )

  // Resolve a library the loaded sample cannot answer for — one remembered from
  // a previous visit, or one that arrived in a link. Only a "not found" from the
  // server proves the selection is really gone; a transport failure must leave
  // the remembered id alone so a flaky network does not quietly reset the scope.
  useEffect(() => {
    if (!pendingLibraryId) return
    let cancelled = false
    const fallBackToAKnownLibrary = () => {
      setPendingLibraryId(null)
      setActiveLibrary((current) => {
        if (current) return current
        const fallback =
          libraries.find((library) => library.workspaceId === activeWorkspace?.id) ?? null
        persistScope({
          ...(activeWorkspace ? { workspaceId: activeWorkspace.id } : {}),
          ...(fallback ? { libraryId: fallback.id } : {}),
        })
        return fallback
      })
    }
    Catalog.getCatalogLibrary({ path: { libraryId: pendingLibraryId } })
      .then((response) => {
        if (cancelled) return
        // Landing the library without moving the workspace to match is what
        // would tear the scope for a link pointing outside the workspace on
        // screen: the shell would read one workspace's configuration under
        // another workspace's name.
        if (!applyLibrarySelection(mapCatalogLibrary(unwrap(response), locale))) {
          fallBackToAKnownLibrary()
        }
      })
      .catch((error: unknown) => {
        if (cancelled) return
        const isGone = error instanceof ApiError && (error.status === 404 || error.status === 403)
        if (!isGone) return
        fallBackToAKnownLibrary()
      })
    return () => {
      cancelled = true
    }
  }, [pendingLibraryId, locale, libraries, activeWorkspace, persistScope, applyLibrarySelection])

  // Resolve session on mount. Bootstrap of the AppContext provider runs
  // before the QueryClientProvider's tree exists for downstream consumers,
  // so this single one-shot fetch stays on the imperative auth API facade
  // intentionally. All other server-state reads flow through useQuery.
  useEffect(() => {
    let cancelled = false
    const resolveInitialSession = async () => {
      try {
        const session = await resolveSessionForStoredScope()
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
      const session = await resolveSessionForStoredScope()
      applySession(session)
    },
    [applySession],
  )

  const clearSignedInState = useCallback(() => {
    enterIdentity(null)
    setUser(null)
    setWorkspaces([])
    setLibraries([])
    setActiveWorkspace(null)
    setActiveLibrary(null)
  }, [enterIdentity])

  const logout = useCallback(async () => {
    // Disarm before the request: a 401 from a request still in flight is then
    // an ordinary failure, not a "your session expired" banner on top of a
    // sign-out the user asked for.
    markSessionSignedOut()
    try {
      await authApi.logout()
    } catch {
      // Ignore logout errors — clear local state regardless
    }
    clearSignedInState()
    setIsBootstrapRequired(false)
    setIsSessionExpired(false)
  }, [clearSignedInState])

  // The single reaction to "the server stopped accepting this session".
  // Dropping the user is what unmounts the authenticated tree, routes to
  // sign-in, and — through the identity boundary in `clearSignedInState` —
  // clears the previous principal's cached server state.
  useEffect(
    () =>
      subscribeToSessionExpired(() => {
        clearSignedInState()
        setIsSessionExpired(true)
      }),
    [clearSignedInState],
  )

  const bootstrapSetup = useCallback(
    async (data: BootstrapSetup) => {
      await authApi.bootstrapSetup(data)
      const session = await resolveSessionForStoredScope()
      applySession(session)
      setIsBootstrapRequired(false)
    },
    [applySession],
  )

  const filteredLibraries = useMemo(
    () => libraries.filter((l) => l.workspaceId === activeWorkspace?.id),
    [libraries, activeWorkspace?.id],
  )

  const persistedSetActiveWorkspace = useCallback(
    (ws: Workspace | null) => {
      setActiveWorkspace(ws)

      // The bootstrap carries a bounded sample of the ACTIVE workspace's
      // libraries, so the loaded set says nothing about the workspace being
      // switched to. Keep the current library only when it already belongs
      // there, otherwise take a locally known one, and fall back to asking
      // the server — which resolves that workspace's library itself.
      const knownLibrary =
        activeLibrary?.workspaceId === ws?.id
          ? activeLibrary
          : (libraries.find((library) => library.workspaceId === ws?.id) ?? null)
      if (knownLibrary) {
        setActiveLibrary(knownLibrary)
        persistScope({
          ...(ws ? { workspaceId: ws.id } : {}),
          libraryId: knownLibrary.id,
        })
        return
      }
      setActiveLibrary(null)
      persistScope(ws ? { workspaceId: ws.id } : {})
      if (ws) {
        refreshSession().catch((error: unknown) => {
          setSessionError(error instanceof Error ? error.message : 'Session resolve failed')
        })
      }
    },
    [activeLibrary, libraries, persistScope, refreshSession],
  )

  const persistedSetActiveLibrary = useCallback(
    (lib: Library | null) => {
      // Every library handed to this setter was picked out of a list the shell
      // had already rendered, so its workspace is discoverable by construction
      // and the selection cannot be refused.
      if (lib) {
        applyLibrarySelection(lib)
        return
      }
      setActiveLibrary(null)
      // With no library the STORED workspace stands. Restating it from
      // render-time state is what put the workspace being left back over the one
      // just chosen when the scope dialog dispatched both from a single click.
      if (!principalId) return
      const { workspaceId } = readScopePreference(principalId)
      writeScopePreference(principalId, workspaceId ? { workspaceId } : {})
    },
    [applyLibrarySelection, principalId],
  )

  /**
   * Land on a library named only by id — what a link carries.
   *
   * The workspace is deliberately not a parameter. Neither caller can know it:
   * a link carries a library id alone, and the workspace it is opened from is
   * not necessarily the one that owns it. The library itself is the only
   * authority, so it is the only thing asked for.
   */
  const selectLibrary = useCallback(
    (libraryId: string): boolean => {
      const knownLibrary = libraries.find((library) => library.id === libraryId) ?? null
      if (knownLibrary) return applyLibrarySelection(knownLibrary)
      // The loaded libraries are a bounded sample, so an id missing from them is
      // not an id that does not exist — on a workspace with thousands of
      // libraries almost none of them are in it. Rejecting the id here is what
      // made a link to a library outside the sample do nothing at all.
      //
      // Nothing is written until the resolver answers: a guessed workspace would
      // store a scope that contradicts itself, and the cost of waiting is only a
      // reload during the lookup falling back to the previous selection.
      setPendingLibraryId(libraryId)
      return true
    },
    [applyLibrarySelection, libraries],
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
      ssoAvailable,
      isLoading,
      sessionError,
      isSessionExpired,
      setUser,
      setWorkspaces,
      setActiveWorkspace: persistedSetActiveWorkspace,
      setLibraries,
      setActiveLibrary: persistedSetActiveLibrary,
      setLocale: persistedSetLocale,
      setIsBootstrapMode,
      setIsBootstrapRequired,
      selectLibrary,
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
      ssoAvailable,
      isLoading,
      sessionError,
      isSessionExpired,
      persistedSetActiveWorkspace,
      persistedSetActiveLibrary,
      persistedSetLocale,
      selectLibrary,
      login,
      logout,
      bootstrapSetup,
      refreshSession,
    ],
  )

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>
}
