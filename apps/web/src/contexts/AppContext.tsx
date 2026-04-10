import React, { createContext, useContext, useState, useCallback, useEffect } from 'react';
import { authApi, ApiError } from '@/api';
import i18n from '@/i18n';
import type { SessionResolveResponse } from '@/api/auth';
import type { User, Workspace, Library, Locale } from '@/types';

interface AppState {
  user: User | null;
  workspaces: Workspace[];
  activeWorkspace: Workspace | null;
  libraries: Library[];
  activeLibrary: Library | null;
  locale: Locale;
  isAuthenticated: boolean;
  isBootstrapMode: boolean;
  isBootstrapRequired: boolean;
  isLoading: boolean;
  sessionError: string | null;
}

interface AppContextValue extends AppState {
  setUser: (user: User | null) => void;
  setWorkspaces: (ws: Workspace[] | ((prev: Workspace[]) => Workspace[])) => void;
  setActiveWorkspace: (ws: Workspace | null) => void;
  setLibraries: (libs: Library[] | ((prev: Library[]) => Library[])) => void;
  setActiveLibrary: (lib: Library | null) => void;
  setLocale: (l: Locale) => void;
  setIsBootstrapMode: (b: boolean) => void;
  setIsBootstrapRequired: (b: boolean) => void;
  login: (login: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  bootstrapSetup: (data: { login: string; password: string; displayName: string; aiSetup?: { providerKind: string; apiKey?: string; baseUrl?: string } }) => Promise<void>;
  refreshSession: () => Promise<void>;
}

const AppContext = createContext<AppContextValue | null>(null);

function preferredLocale(sessionLocale: Locale): Locale {
  const savedLocale = localStorage.getItem('rustrag_locale');
  return savedLocale || sessionLocale;
}

function localizedShellName(
  slug: string,
  fallbackName: string,
  locale: Locale,
): string {
  if (slug === 'default') {
    return i18n.t('shell.defaultWorkspaceLabel', { lng: locale });
  }
  if (slug === 'default-library') {
    return i18n.t('shell.defaultLibraryLabel', { lng: locale });
  }
  return fallbackName;
}

function mapSessionToState(session: SessionResolveResponse, locale: Locale) {
  let user: User | null = null;
  if (session.me) {
    user = {
      id: session.me.principal.id,
      login: session.me.user?.login ?? session.me.principal.displayLabel,
      displayName: session.me.user?.displayName ?? session.me.principal.displayLabel,
      accessLabel: session.me.principal.displayLabel,
      role: 'admin',
    };
  }

  const workspaces: Workspace[] = (session.shellBootstrap?.workspaces ?? []).map(ws => ({
    id: ws.id,
    name: localizedShellName(ws.slug, ws.name, locale),
    createdAt: '',
  }));

  const libraries: Library[] = (session.shellBootstrap?.libraries ?? []).map(lib => ({
    id: lib.id,
    workspaceId: lib.workspaceId,
    name: localizedShellName(lib.slug, lib.name, locale),
    createdAt: '',
    ingestionReady: lib.ingestionReady,
    queryReady: lib.missingBindingPurposes.length === 0 && lib.ingestionReady,
    missingBindingPurposes: lib.missingBindingPurposes as Library['missingBindingPurposes'],
  }));

  const isBootstrapRequired = session.mode === 'bootstrap' ||
    (session.bootstrapStatus?.setupRequired ?? false);

  return { user, workspaces, libraries, isBootstrapRequired, locale };
}

export function AppProvider({ children }: { children: React.ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [activeWorkspace, setActiveWorkspace] = useState<Workspace | null>(null);
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [activeLibrary, setActiveLibrary] = useState<Library | null>(null);
  const [locale, setLocaleRaw] = useState<Locale>('en');
  const setLocale = useCallback((l: Locale) => {
    setLocaleRaw(l);
    i18n.changeLanguage(l);
    localStorage.setItem('rustrag_locale', l);
  }, []);
  const [isBootstrapMode, setIsBootstrapMode] = useState(false);
  const [isBootstrapRequired, setIsBootstrapRequired] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [sessionError, setSessionError] = useState<string | null>(null);

  const applySession = useCallback((session: SessionResolveResponse) => {
    const resolvedLocale = preferredLocale(session.locale || 'en');
    const state = mapSessionToState(session, resolvedLocale);
    setUser(state.user);
    setWorkspaces(state.workspaces);
    setLibraries(state.libraries);
    setIsBootstrapRequired(state.isBootstrapRequired);
    setLocale(state.locale);

    const savedWsId = localStorage.getItem('rustrag_active_workspace');
    const savedLibId = localStorage.getItem('rustrag_active_library');

    if (state.workspaces.length > 0) {
      setActiveWorkspace(prev => {
        const match = prev && state.workspaces.find(w => w.id === prev.id);
        if (match) return prev;
        const saved = savedWsId ? state.workspaces.find(w => w.id === savedWsId) : null;
        return saved ?? state.workspaces[0];
      });
    } else {
      setActiveWorkspace(null);
    }

    if (state.libraries.length > 0) {
      setActiveLibrary(prev => {
        const match = prev && state.libraries.find(l => l.id === prev.id);
        if (match) return prev;
        const saved = savedLibId ? state.libraries.find(l => l.id === savedLibId) : null;
        return saved ?? state.libraries[0];
      });
    } else {
      setActiveLibrary(null);
    }
  }, []);

  // Resolve session on mount
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const session = await authApi.resolveSession();
        if (!cancelled) {
          applySession(session);
          setSessionError(null);
        }
      } catch (err) {
        if (!cancelled) {
          if (err instanceof ApiError && err.status === 401) {
            // Not authenticated — expected on first visit
            setUser(null);
          } else {
            setSessionError(err instanceof Error ? err.message : 'Session resolve failed');
          }
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [applySession]);

  const login = useCallback(async (loginVal: string, password: string) => {
    await authApi.login(loginVal, password);
    const session = await authApi.resolveSession();
    applySession(session);
  }, [applySession]);

  const logout = useCallback(async () => {
    try {
      await authApi.logout();
    } catch {
      // Ignore logout errors — clear local state regardless
    }
    setUser(null);
    setWorkspaces([]);
    setLibraries([]);
    setActiveWorkspace(null);
    setActiveLibrary(null);
    setIsBootstrapRequired(false);
  }, []);

  const bootstrapSetup = useCallback(async (data: { login: string; password: string; displayName: string; aiSetup?: { providerKind: string; apiKey?: string; baseUrl?: string } }) => {
    await authApi.bootstrapSetup(data);
    const session = await authApi.resolveSession();
    applySession(session);
    setIsBootstrapRequired(false);
  }, [applySession]);

  const refreshSession = useCallback(async () => {
    const session = await authApi.resolveSession();
    applySession(session);
  }, [applySession]);

  const filteredLibraries = libraries.filter(l => l.workspaceId === activeWorkspace?.id);

  const persistedSetActiveWorkspace = useCallback((ws: Workspace | null) => {
    setActiveWorkspace(ws);
    if (ws) localStorage.setItem('rustrag_active_workspace', ws.id);
    else localStorage.removeItem('rustrag_active_workspace');
  }, []);

  const persistedSetActiveLibrary = useCallback((lib: Library | null) => {
    setActiveLibrary(lib);
    if (lib) localStorage.setItem('rustrag_active_library', lib.id);
    else localStorage.removeItem('rustrag_active_library');
  }, []);

  const value: AppContextValue = {
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
    setLocale,
    setIsBootstrapMode,
    setIsBootstrapRequired,
    login,
    logout,
    bootstrapSetup,
    refreshSession,
  };

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp() {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useApp must be used within AppProvider');
  return ctx;
}
