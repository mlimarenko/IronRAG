import { Iam } from './generated'
import { unwrap } from './runtime'
import type {
  BootstrapSetupRequest,
  BootstrapStatus,
  LoginSessionRequest,
  SessionResolveResponse,
  SessionResponse,
} from './generated'

export type BootstrapSetup = BootstrapSetupRequest
export type { BootstrapProviderBindingBundle } from './generated'
export type { BootstrapStatus, SessionResolveResponse, SessionResponse }

export interface SessionSelection {
  activeWorkspaceId?: string
  activeLibraryId?: string
}

function unwrapRequired<T>(
  result: { data?: T | undefined; error?: unknown; response?: Response | undefined },
  operation: string,
): T {
  const data = unwrap(result)
  if (data === undefined) {
    throw new Error(`${operation} returned no response body`)
  }
  return data
}

export const authApi = {
  getIamBootstrap: (): Promise<BootstrapStatus> =>
    Iam.getIamBootstrap().then((result) =>
      unwrapRequired<BootstrapStatus>(result, 'getIamBootstrap'),
    ),
  resolveSession: (selection: SessionSelection = {}): Promise<SessionResolveResponse> =>
    Iam.getIamContext({ query: selection }).then((result) =>
      unwrapRequired<SessionResolveResponse>(result, 'resolveSession'),
    ),
  login: (login: string, password: string): Promise<SessionResponse> => {
    const body: LoginSessionRequest = { login, password }
    return Iam.createIamSession({ body }).then((result) =>
      unwrapRequired<SessionResponse>(result, 'login'),
    )
  },
  logout: () =>
    Iam.deleteIamSession().then((result) => {
      unwrap(result)
    }),
  bootstrapSetup: (data: BootstrapSetupRequest): Promise<SessionResponse> =>
    Iam.createIamBootstrap({ body: data }).then((result) =>
      unwrapRequired<SessionResponse>(result, 'bootstrapSetup'),
    ),
}
