import { createContext, useContext } from 'react';

import type { BootstrapSetup } from '@/shared/api/auth';
import type { Library, Locale, User, Workspace } from '@/shared/types';

export interface AppState {
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

export interface AppContextValue extends AppState {
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
  bootstrapSetup: (data: BootstrapSetup) => Promise<void>;
  refreshSession: () => Promise<void>;
}

export const AppContext = createContext<AppContextValue | null>(null);

export function useApp() {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useApp must be used within AppProvider');
  return ctx;
}
