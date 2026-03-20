import type { LoginPayload, UiSessionResponse } from 'src/models/ui/auth'
import { apiHttp, unwrap } from './http'

interface RawSessionUser {
  principalId: string
  email: string
  displayName: string
}

interface RawSessionResponse {
  sessionId: string
  expiresAt: string
  user: RawSessionUser
}

function mapSession(response: RawSessionResponse): UiSessionResponse {
  return {
    sessionId: response.sessionId,
    expiresAt: response.expiresAt,
    user: {
      principalId: response.user.principalId,
      email: response.user.email,
      displayName: response.user.displayName,
    },
  }
}

export async function fetchSession(): Promise<UiSessionResponse | null> {
  const response = await apiHttp.get<RawSessionResponse>('/iam/session', {
    validateStatus: (status) => status === 200 || status === 401,
  })
  if (response.status === 401) {
    return null
  }
  return mapSession(response.data)
}

export async function loginWithPassword(payload: LoginPayload): Promise<UiSessionResponse> {
  return mapSession(
    await unwrap(
      apiHttp.post<RawSessionResponse>('/iam/session/login', {
        email: payload.login,
        password: payload.password,
        rememberMe: payload.rememberMe,
      }),
    ),
  )
}

export async function logout(): Promise<void> {
  await apiHttp.post('/iam/session/logout')
}
