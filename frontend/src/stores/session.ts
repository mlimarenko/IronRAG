import { defineStore } from 'pinia'
import { i18n } from 'src/lib/i18n'
import type {
  BootstrapAiSetupDescriptor,
  BootstrapSetupPayload,
  LoginPayload,
  UiSessionResponse,
} from 'src/models/ui/auth'
import {
  completeBootstrapSetup,
  fetchBootstrapStatus,
  fetchSession,
  loginWithPassword,
  logout as logoutRequest,
} from 'src/services/api/auth'

const LOCALE_STORAGE_KEY = 'rustrag.ui.locale'

function readStoredLocale(): 'en' | 'ru' {
  if (typeof window === 'undefined') {
    return 'ru'
  }
  const stored = window.localStorage.getItem(LOCALE_STORAGE_KEY)
  return stored === 'en' || stored === 'ru' ? stored : 'ru'
}

function writeStoredLocale(locale: 'en' | 'ru') {
  if (typeof window === 'undefined') {
    return
  }
  window.localStorage.setItem(LOCALE_STORAGE_KEY, locale)
}

interface SessionState {
  session: UiSessionResponse | null
  locale: 'en' | 'ru'
  error: string | null
  status: 'idle' | 'loading' | 'ready' | 'guest' | 'setup_required'
  bootstrapSetupRequired: boolean | null
  bootstrapAiSetup: BootstrapAiSetupDescriptor | null
}

export const useSessionStore = defineStore('session', {
  state: (): SessionState => ({
    session: null,
    locale: readStoredLocale(),
    error: null,
    status: 'idle',
    bootstrapSetupRequired: null,
    bootstrapAiSetup: null,
  }),
  getters: {
    isAuthenticated: (state) => state.session !== null,
    user: (state) => state.session?.user ?? null,
    requiresBootstrapSetup: (state) => state.bootstrapSetupRequired === true,
  },
  actions: {
    async resolveBootstrapStatus(): Promise<boolean> {
      const status = await fetchBootstrapStatus()
      this.bootstrapSetupRequired = status.setupRequired
      this.bootstrapAiSetup = status.setupRequired ? status.aiSetup : null
      return status.setupRequired
    },
    async restoreSession(): Promise<void> {
      if (this.status === 'loading' || this.status === 'ready') {
        return
      }
      this.status = 'loading'
      this.error = null
      try {
        const session = await fetchSession()
        if (!session) {
          this.session = null
          const setupRequired = await this.resolveBootstrapStatus()
          this.status = setupRequired ? 'setup_required' : 'guest'
          return
        }
        this.session = session
        this.bootstrapSetupRequired = false
        this.bootstrapAiSetup = null
        this.status = 'ready'
      } catch {
        this.session = null
        this.bootstrapSetupRequired = null
        this.bootstrapAiSetup = null
        this.status = 'guest'
      }
    },
    async loginWithPassword(payload: LoginPayload): Promise<void> {
      this.status = 'loading'
      this.error = null
      try {
        this.session = await loginWithPassword(payload)
        this.setLocale(payload.locale)
        this.bootstrapSetupRequired = false
        this.bootstrapAiSetup = null
        this.status = 'ready'
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to sign in'
        this.status = 'guest'
        throw error
      }
    },
    async completeBootstrapSetup(payload: BootstrapSetupPayload): Promise<void> {
      this.status = 'loading'
      this.error = null
      try {
        this.session = await completeBootstrapSetup(payload)
        this.setLocale(payload.locale)
        this.bootstrapSetupRequired = false
        this.bootstrapAiSetup = null
        this.status = 'ready'
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to complete setup'
        try {
          const setupRequired = await this.resolveBootstrapStatus()
          this.status = setupRequired ? 'setup_required' : 'guest'
        } catch {
          this.bootstrapSetupRequired = null
          this.status = 'guest'
        }
        throw error
      }
    },
    async logout(): Promise<void> {
      await logoutRequest()
      this.session = null
      this.bootstrapSetupRequired = false
      this.bootstrapAiSetup = null
      this.status = 'guest'
    },
    setLocale(locale: 'en' | 'ru') {
      this.locale = locale
      writeStoredLocale(locale)
      i18n.global.locale.value = locale
    },
  },
})
