import type {
  BootstrapSetupPayload,
  BootstrapStatusResponse,
  LoginPayload,
  UiSessionResponse,
} from 'src/models/ui/auth'
import { apiHttp, unwrap } from './http'

interface RawSessionUser {
  principalId: string
  login: string
  email: string
  displayName: string
}

interface RawSessionResponse {
  sessionId: string
  expiresAt: string
  user: RawSessionUser
}

interface RawBootstrapStatusResponse {
  setupRequired: boolean
}

function mapSession(response: RawSessionResponse): UiSessionResponse {
  return {
    sessionId: response.sessionId,
    expiresAt: response.expiresAt,
    user: {
      principalId: response.user.principalId,
      login: response.user.login,
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
        login: payload.login,
        password: payload.password,
        rememberMe: payload.rememberMe,
      }),
    ),
  )
}

export async function fetchBootstrapStatus(): Promise<BootstrapStatusResponse> {
  return unwrap(apiHttp.get<RawBootstrapStatusResponse>('/iam/bootstrap/status'))
}

export async function completeBootstrapSetup(
  payload: BootstrapSetupPayload,
): Promise<UiSessionResponse> {
  return mapSession(
    await unwrap(
      apiHttp.post<RawSessionResponse>('/iam/bootstrap/setup', {
        login: payload.login,
        displayName: payload.displayName,
        password: payload.password,
      }),
    ),
  )
}

export async function logout(): Promise<void> {
  await apiHttp.post('/iam/session/logout')
}
