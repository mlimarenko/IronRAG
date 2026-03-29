export type UiLocale = 'en' | 'ru'

export interface SessionUser {
  principalId: string
  login: string
  email: string
  displayName: string
}

export interface UiSessionResponse {
  sessionId: string
  expiresAt: string
  user: SessionUser
}

export interface LoginPayload {
  login: string
  password: string
  rememberMe: boolean
  locale: UiLocale
}

export interface BootstrapStatusResponse {
  setupRequired: boolean
}

export interface BootstrapSetupPayload {
  login: string
  displayName: string
  password: string
  locale: UiLocale
}
