export type UiLocale = 'en' | 'ru'

export interface SessionUser {
  principalId: string
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
