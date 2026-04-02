import { defineStore } from 'pinia'
import type {
  LibraryOption,
  ShellCapabilities,
  ShellContextResponse,
  ShellCurrentUser,
  ShellGrant,
  ShellWorkspaceMembership,
  WorkspaceOption,
} from 'src/models/ui/shell'
import {
  buildShellContext,
  createLibrary,
  createWorkspace,
  deleteLibrary,
  deleteWorkspace,
  fetchLibrariesForWorkspace,
  fetchShellBootstrap,
} from 'src/services/api/shell'
import { useSessionStore } from './session'

const ACTIVE_WORKSPACE_STORAGE_KEY = 'rustrag.shell.activeWorkspaceId'
const ACTIVE_LIBRARY_STORAGE_KEY = 'rustrag.shell.activeLibraryId'

interface ShellState {
  context: ShellContextResponse | null
  viewer: ShellCurrentUser | null
  workspaces: WorkspaceOption[]
  libraries: LibraryOption[]
  activeWorkspace: WorkspaceOption | null
  activeLibrary: LibraryOption | null
  workspaceMemberships: ShellWorkspaceMembership[]
  effectiveGrants: ShellGrant[]
  capabilities: ShellCapabilities | null
  loading: boolean
  error: string | null
  showCreateWorkspace: boolean
  showCreateLibrary: boolean
  showDeleteWorkspace: boolean
  deleteWorkspaceTarget: WorkspaceOption | null
  showDeleteLibrary: boolean
  deleteLibraryTarget: LibraryOption | null
}

function readStoredSelection(key: string): string | null {
  if (typeof window === 'undefined') {
    return null
  }
  return window.localStorage.getItem(key)
}

function writeStoredSelection(key: string, value: string | null): void {
  if (typeof window === 'undefined') {
    return
  }
  if (!value) {
    window.localStorage.removeItem(key)
    return
  }
  window.localStorage.setItem(key, value)
}

function deriveLibraryCreateCapability(
  capabilities: ShellCapabilities | null,
  workspaceId: string | null,
): boolean {
  if (!capabilities || !workspaceId) {
    return false
  }

  return capabilities.canCreateWorkspace || capabilities.creatableWorkspaceIds.includes(workspaceId)
}

export const useShellStore = defineStore('shell', {
  state: (): ShellState => ({
    context: null,
    viewer: null,
    workspaces: [],
    libraries: [],
    activeWorkspace: null,
    activeLibrary: null,
    workspaceMemberships: [],
    effectiveGrants: [],
    capabilities: null,
    loading: false,
    error: null,
    showCreateWorkspace: false,
    showCreateLibrary: false,
    showDeleteWorkspace: false,
    deleteWorkspaceTarget: null,
    showDeleteLibrary: false,
    deleteLibraryTarget: null,
  }),
  getters: {
    currentUser: (state) => state.viewer,
    adminEnabled: (state) => state.capabilities?.adminEnabled ?? false,
    canCreateWorkspace: (state) => state.capabilities?.canCreateWorkspace ?? false,
    canCreateLibrary: (state) =>
      deriveLibraryCreateCapability(state.capabilities, state.activeWorkspace?.id ?? null),
    hasWorkspaceOptions: (state) => state.workspaces.length > 0,
    hasLibraryOptions: (state) => state.libraries.length > 0,
    hasBootstrapResources(): boolean {
      return this.hasWorkspaceOptions && this.hasLibraryOptions
    },
  },
  actions: {
    clearContext(): void {
      this.context = null
      this.viewer = null
      this.workspaces = []
      this.libraries = []
      this.activeWorkspace = null
      this.activeLibrary = null
      this.workspaceMemberships = []
      this.effectiveGrants = []
      this.capabilities = null
      this.error = null
      this.loading = false
      this.showCreateWorkspace = false
      this.showCreateLibrary = false
      this.showDeleteWorkspace = false
      this.deleteWorkspaceTarget = null
      this.showDeleteLibrary = false
      this.deleteLibraryTarget = null
      writeStoredSelection(ACTIVE_WORKSPACE_STORAGE_KEY, null)
      writeStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY, null)
    },
    syncClientLocale(locale: 'en' | 'ru'): void {
      const sessionStore = useSessionStore()
      sessionStore.setLocale(locale)
    },
    refreshContext(locale: 'en' | 'ru'): void {
      if (!this.viewer || !this.capabilities) {
        this.context = null
        return
      }

      this.context = buildShellContext(
        {
          currentUser: this.viewer,
          workspaces: this.workspaces,
          activeWorkspace: this.activeWorkspace,
          libraries: this.libraries,
          activeLibrary: this.activeLibrary,
          workspaceMemberships: this.workspaceMemberships,
          effectiveGrants: this.effectiveGrants,
          capabilities: this.capabilities,
        },
        locale,
      )
    },
    async loadContext(options?: {
      preferredWorkspaceId?: string | null
      preferredLibraryId?: string | null
    }): Promise<void> {
      const sessionStore = useSessionStore()
      this.loading = true
      try {
        const preferredWorkspaceId =
          options?.preferredWorkspaceId ?? readStoredSelection(ACTIVE_WORKSPACE_STORAGE_KEY) ?? null
        const preferredLibraryId =
          options?.preferredLibraryId ?? readStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY) ?? null
        const bootstrap = await fetchShellBootstrap({
          preferredWorkspaceId,
          preferredLibraryId,
        })

        this.viewer = bootstrap.currentUser
        this.workspaces = bootstrap.workspaces
        this.activeWorkspace = bootstrap.activeWorkspace
        this.libraries = bootstrap.libraries
        this.activeLibrary = bootstrap.activeLibrary
        this.workspaceMemberships = bootstrap.workspaceMemberships
        this.effectiveGrants = bootstrap.effectiveGrants
        this.capabilities = bootstrap.capabilities
        writeStoredSelection(ACTIVE_WORKSPACE_STORAGE_KEY, this.activeWorkspace?.id ?? null)
        writeStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY, this.activeLibrary?.id ?? null)
        this.refreshContext(sessionStore.locale)
        this.syncClientLocale(sessionStore.locale)
        this.error = null
      } catch (error) {
        this.context = null
        this.error = error instanceof Error ? error.message : 'Failed to load shell context'
        throw error
      } finally {
        this.loading = false
      }
    },
    async switchWorkspace(workspaceId: string): Promise<void> {
      const sessionStore = useSessionStore()
      const workspace = this.workspaces.find((item) => item.id === workspaceId) ?? null

      this.activeWorkspace = workspace
      writeStoredSelection(ACTIVE_WORKSPACE_STORAGE_KEY, workspace?.id ?? null)
      writeStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY, null)

      if (!workspace) {
        this.libraries = []
        this.activeLibrary = null
        this.refreshContext(sessionStore.locale)
        return
      }

      try {
        this.libraries = await fetchLibrariesForWorkspace(workspace.id)
        this.activeLibrary = this.libraries[0] ?? null
        const activeLibraryId = this.libraries[0]?.id ?? null
        writeStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY, activeLibraryId)
        this.refreshContext(sessionStore.locale)
        this.error = null
      } catch (error) {
        this.activeLibrary = null
        this.context = null
        this.error = error instanceof Error ? error.message : 'Failed to switch workspace'
      }
    },
    switchLibrary(libraryId: string): void {
      const sessionStore = useSessionStore()
      this.activeLibrary = this.libraries.find((item) => item.id === libraryId) ?? null
      const activeLibraryId = this.activeLibrary ? this.activeLibrary.id : null
      writeStoredSelection(ACTIVE_LIBRARY_STORAGE_KEY, activeLibraryId)
      this.refreshContext(sessionStore.locale)
      this.error = null
    },
    switchLocale(locale: 'en' | 'ru'): void {
      this.syncClientLocale(locale)
      this.refreshContext(locale)
      this.error = null
    },
    async submitWorkspace(name: string): Promise<void> {
      if (!this.capabilities?.canCreateWorkspace) {
        this.error = 'You do not have permission to create a workspace'
        return
      }

      try {
        const workspace = await createWorkspace(name)
        this.showCreateWorkspace = false
        await this.loadContext({ preferredWorkspaceId: workspace.id })
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create workspace'
      }
    },
    async submitLibrary(name: string): Promise<void> {
      if (!this.activeWorkspace) {
        this.error = 'Select a workspace before creating a library'
        return
      }
      if (!deriveLibraryCreateCapability(this.capabilities, this.activeWorkspace.id)) {
        this.error = 'You do not have permission to create a library in this workspace'
        return
      }

      try {
        const library = await createLibrary({
          workspaceId: this.activeWorkspace.id,
          name,
        })
        this.showCreateLibrary = false
        await this.loadContext({
          preferredWorkspaceId: this.activeWorkspace.id,
          preferredLibraryId: library.id,
        })
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create library'
      }
    },
    requestDeleteWorkspace(workspace: WorkspaceOption): void {
      this.deleteWorkspaceTarget = workspace
      this.showDeleteWorkspace = true
    },
    async confirmDeleteWorkspace(): Promise<void> {
      const target = this.deleteWorkspaceTarget
      if (!target) return
      try {
        await deleteWorkspace(target.id)
        this.showDeleteWorkspace = false
        this.deleteWorkspaceTarget = null
        await this.loadContext()
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to delete workspace'
      }
    },
    cancelDeleteWorkspace(): void {
      this.showDeleteWorkspace = false
      this.deleteWorkspaceTarget = null
    },
    requestDeleteLibrary(library: LibraryOption): void {
      this.deleteLibraryTarget = library
      this.showDeleteLibrary = true
    },
    async confirmDeleteLibrary(): Promise<void> {
      const target = this.deleteLibraryTarget
      if (!target) return
      try {
        await deleteLibrary(target.workspaceId, target.id)
        this.showDeleteLibrary = false
        this.deleteLibraryTarget = null
        await this.loadContext({ preferredWorkspaceId: this.activeWorkspace?.id ?? undefined })
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to delete library'
      }
    },
    cancelDeleteLibrary(): void {
      this.showDeleteLibrary = false
      this.deleteLibraryTarget = null
    },
  },
})
