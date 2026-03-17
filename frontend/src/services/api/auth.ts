import type { LoginPayload, UiSessionResponse } from 'src/models/ui/auth'
import { apiHttp, unwrap } from './http'

interface RawSessionUser {
  id: string
  email: string
  display_name: string
  role_label: string
  initials: string
}

interface RawSessionResponse {
  session_id: string
  locale: 'en' | 'ru'
  active_workspace_id: string | null
  active_library_id: string | null
  expires_at: string
  user: RawSessionUser
}

function mapSession(response: RawSessionResponse): UiSessionResponse {
  return {
    sessionId: response.session_id,
    locale: response.locale,
    activeWorkspaceId: response.active_workspace_id,
    activeLibraryId: response.active_library_id,
    expiresAt: response.expires_at,
    user: {
      id: response.user.id,
      email: response.user.email,
      displayName: response.user.display_name,
      roleLabel: response.user.role_label,
      initials: response.user.initials,
    },
  }
}

export async function fetchSession(): Promise<UiSessionResponse | null> {
  const response = await apiHttp.get<RawSessionResponse>('/ui/auth/session', {
    validateStatus: (status) => status === 200 || status === 204,
  })
  if (response.status === 204) {
    return null
  }
  return mapSession(response.data)
}

export async function loginWithPassword(payload: LoginPayload): Promise<UiSessionResponse> {
  return mapSession(
    await unwrap(
      apiHttp.post<RawSessionResponse>('/ui/auth/login', {
        login: payload.login,
        password: payload.password,
        remember_me: payload.rememberMe,
        locale: payload.locale,
      }),
    ),
  )
}

export async function logout(): Promise<void> {
  await unwrap(apiHttp.post<{ ok: boolean }>('/ui/auth/logout'))
}
