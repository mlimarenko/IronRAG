import { defineStore } from 'pinia'
import type { ShellContextResponse } from 'src/models/ui/shell'
import {
  createLibrary,
  createWorkspace,
  fetchShellContext,
  updateShellContext,
} from 'src/services/api/shell'
import { useSessionStore } from './session'

interface ShellState {
  context: ShellContextResponse | null
  loading: boolean
  error: string | null
  showCreateWorkspace: boolean
  showCreateLibrary: boolean
}

export const useShellStore = defineStore('shell', {
  state: (): ShellState => ({
    context: null,
    loading: false,
    error: null,
    showCreateWorkspace: false,
    showCreateLibrary: false,
  }),
  getters: {
    currentUser: (state) => state.context?.currentUser ?? null,
  },
  actions: {
    clearContext(): void {
      this.context = null
      this.error = null
      this.loading = false
      this.showCreateWorkspace = false
      this.showCreateLibrary = false
    },
    syncClientLocale(locale: 'en' | 'ru'): void {
      const sessionStore = useSessionStore()
      sessionStore.setLocale(locale)
    },
    async loadContext(): Promise<void> {
      this.loading = true
      try {
        this.context = await fetchShellContext()
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load workspace context'
        throw error
      } finally {
        this.loading = false
      }
    },
    async switchWorkspace(workspaceId: string): Promise<void> {
      try {
        this.context = await updateShellContext({ workspaceId })
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to switch workspace'
      }
    },
    async switchLibrary(libraryId: string): Promise<void> {
      try {
        this.context = await updateShellContext({ libraryId })
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to switch library'
      }
    },
    async switchLocale(locale: 'en' | 'ru'): Promise<void> {
      try {
        this.context = await updateShellContext({ locale })
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to switch locale'
      }
    },
    async submitWorkspace(name: string): Promise<void> {
      try {
        const workspace = await createWorkspace(name)
        this.showCreateWorkspace = false
        this.context = await updateShellContext({ workspaceId: workspace.id })
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create workspace'
      }
    },
    async submitLibrary(name: string): Promise<void> {
      if (!this.context) {
        return
      }
      try {
        const library = await createLibrary({
          workspaceId: this.context.activeWorkspace.id,
          name,
        })
        this.showCreateLibrary = false
        this.context = await updateShellContext({ libraryId: library.id })
        this.syncClientLocale(this.context.locale)
        this.error = null
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to create library'
      }
    },
  },
})
