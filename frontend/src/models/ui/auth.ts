export type UiLocale = 'en' | 'ru'

export interface SessionUser {
  id: string
  email: string
  displayName: string
  roleLabel: string
  initials: string
}

export interface UiSessionResponse {
  sessionId: string
  locale: UiLocale
  activeWorkspaceId: string | null
  activeLibraryId: string | null
  expiresAt: string
  user: SessionUser
}

export interface LoginPayload {
  login: string
  password: string
  rememberMe: boolean
  locale: UiLocale
}
