import { Iam } from "./generated";
import { ApiError, type ApiErrorBody, unwrap } from "./runtime";
import type {
  BootstrapProviderBindingBundle,
  BootstrapSetupRequest,
  BootstrapStatus,
  LoginSessionRequest,
  SessionResolveResponse,
  SessionResponse,
} from "./generated";

export type BootstrapSetup = BootstrapSetupRequest;
export type { BootstrapProviderBindingBundle, BootstrapStatus, SessionResolveResponse, SessionResponse };

function unwrapRequired<T>(
  result: { data?: T | undefined; error?: unknown; response?: Response | undefined },
  operation: string,
): T {
  if (result.error !== undefined && result.error !== null) {
    const status = result.response?.status ?? 0;
    const body: ApiErrorBody =
      typeof result.error === "object" && result.error !== null
        ? { ...result.error }
        : { error: String(result.error) };
    throw new ApiError(status, body);
  }
  if (result.data === undefined) {
    throw new Error(`${operation} returned no response body`);
  }
  return result.data;
}

export const authApi = {
  getBootstrapStatus: (): Promise<BootstrapStatus> =>
    Iam.getBootstrapStatus().then((result) => unwrapRequired<BootstrapStatus>(result, "getBootstrapStatus")),
  resolveSession: (): Promise<SessionResolveResponse> =>
    Iam.resolveIamSession().then((result) => unwrapRequired<SessionResolveResponse>(result, "resolveSession")),
  login: (login: string, password: string): Promise<SessionResponse> => {
    const body: LoginSessionRequest = { login, password };
    return Iam.loginIamSession({ body }).then((result) => unwrapRequired<SessionResponse>(result, "login"));
  },
  logout: () =>
    Iam.logoutIamSession().then((result) => {
      unwrap(result);
    }),
  bootstrapSetup: (data: BootstrapSetupRequest): Promise<SessionResponse> =>
    Iam.postBootstrapSetup({ body: data }).then((result) => unwrapRequired<SessionResponse>(result, "bootstrapSetup")),
};
