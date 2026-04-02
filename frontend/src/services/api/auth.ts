import type {
  BootstrapAiSetupDescriptor,
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
  aiSetup: RawBootstrapAiSetupResponse | null
}

interface RawBootstrapProviderDescriptor {
  id: string
  providerKind: string
  displayName: string
  apiStyle: string
  lifecycleState: string
  credentialSource: 'missing' | 'env'
}

interface RawBootstrapModelDescriptor {
  id: string
  providerCatalogId: string
  modelName: string
  capabilityKind: string
  modalityKind: string
  allowedBindingPurposes: ('extract_graph' | 'embed_chunk' | 'query_answer' | 'vision')[]
  contextWindow: number | null
  maxOutputTokens: number | null
}

interface RawBootstrapBindingSelectionDescriptor {
  bindingPurpose: 'extract_graph' | 'embed_chunk' | 'query_answer' | 'vision'
  providerKind: string | null
  modelCatalogId: string | null
  configured: boolean
}

interface RawBootstrapAiSetupResponse {
  providers: RawBootstrapProviderDescriptor[]
  models: RawBootstrapModelDescriptor[]
  bindingSelections: RawBootstrapBindingSelectionDescriptor[]
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

function mapBootstrapAiSetup(response: RawBootstrapAiSetupResponse): BootstrapAiSetupDescriptor {
  return {
    providers: response.providers.map((provider) => ({
      providerCatalogId: provider.id,
      providerKind: provider.providerKind,
      displayName: provider.displayName,
      apiStyle: provider.apiStyle,
      lifecycleState: provider.lifecycleState,
      credentialSource: provider.credentialSource,
    })),
    models: response.models.map((model) => ({
      id: model.id,
      providerCatalogId: model.providerCatalogId,
      modelName: model.modelName,
      capabilityKind: model.capabilityKind,
      modalityKind: model.modalityKind,
      allowedBindingPurposes: model.allowedBindingPurposes,
      contextWindow: model.contextWindow,
      maxOutputTokens: model.maxOutputTokens,
    })),
    bindingSelections: response.bindingSelections.map((selection) => ({
      bindingPurpose: selection.bindingPurpose,
      providerKind: selection.providerKind,
      modelCatalogId: selection.modelCatalogId,
      configured: selection.configured,
    })),
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
  const response = await unwrap(apiHttp.get<RawBootstrapStatusResponse>('/iam/bootstrap/status'))
  return {
    setupRequired: response.setupRequired,
    aiSetup: response.aiSetup ? mapBootstrapAiSetup(response.aiSetup) : null,
  }
}

export async function completeBootstrapSetup(
  payload: BootstrapSetupPayload,
): Promise<UiSessionResponse> {
  return mapSession(
    await unwrap(
      apiHttp.post<RawSessionResponse>('/iam/bootstrap/setup', {
        login: payload.login,
        displayName: payload.displayName.trim() || null,
        password: payload.password,
        aiSetup: payload.aiSetup
          ? {
              credentials: payload.aiSetup.credentials,
              bindingSelections: payload.aiSetup.bindingSelections,
            }
          : null,
      }),
    ),
  )
}

export async function logout(): Promise<void> {
  await apiHttp.post('/iam/session/logout')
}
