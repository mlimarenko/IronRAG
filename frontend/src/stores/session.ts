import { defineStore } from 'pinia'
import { i18n } from 'src/lib/i18n'
import type { LoginPayload, UiSessionResponse } from 'src/models/ui/auth'
import { fetchSession, loginWithPassword, logout as logoutRequest } from 'src/services/api/auth'

interface SessionState {
  session: UiSessionResponse | null
  locale: 'en' | 'ru'
  error: string | null
  status: 'idle' | 'loading' | 'ready' | 'guest'
}

export const useSessionStore = defineStore('session', {
  state: (): SessionState => ({
    session: null,
    locale: 'ru',
    error: null,
    status: 'idle',
  }),
  getters: {
    isAuthenticated: (state) => state.session !== null,
    user: (state) => state.session?.user ?? null,
  },
  actions: {
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
          this.status = 'guest'
          return
        }
        this.session = session
        this.status = 'ready'
      } catch {
        this.session = null
        this.status = 'guest'
      }
    },
    async loginWithPassword(payload: LoginPayload): Promise<void> {
      this.status = 'loading'
      this.error = null
      try {
        this.session = await loginWithPassword(payload)
        this.locale = payload.locale
        this.status = 'ready'
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to sign in'
        this.status = 'guest'
        throw error
      }
    },
    async logout(): Promise<void> {
      await logoutRequest()
      this.session = null
      this.status = 'guest'
    },
    setLocale(locale: 'en' | 'ru') {
      this.locale = locale
      i18n.global.locale.value = locale
    },
  },
})
