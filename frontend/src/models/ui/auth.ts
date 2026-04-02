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

export type BootstrapBindingPurpose = 'extract_graph' | 'embed_chunk' | 'query_answer' | 'vision'

export type BootstrapCredentialSource = 'missing' | 'env'

export interface BootstrapProviderDescriptor {
  providerCatalogId: string
  providerKind: string
  displayName: string
  apiStyle: string
  lifecycleState: string
  credentialSource: BootstrapCredentialSource
}

export interface BootstrapModelDescriptor {
  id: string
  providerCatalogId: string
  modelName: string
  capabilityKind: string
  modalityKind: string
  allowedBindingPurposes: BootstrapBindingPurpose[]
  contextWindow: number | null
  maxOutputTokens: number | null
}

export interface BootstrapBindingSelectionDescriptor {
  bindingPurpose: BootstrapBindingPurpose
  providerKind: string | null
  modelCatalogId: string | null
  configured: boolean
}

export interface BootstrapAiSetupDescriptor {
  providers: BootstrapProviderDescriptor[]
  models: BootstrapModelDescriptor[]
  bindingSelections: BootstrapBindingSelectionDescriptor[]
}

export interface BootstrapStatusResponse {
  setupRequired: boolean
  aiSetup: BootstrapAiSetupDescriptor | null
}

export interface BootstrapCredentialInput {
  providerKind: string
  apiKey: string | null
}

export interface BootstrapBindingInput {
  bindingPurpose: BootstrapBindingPurpose
  providerKind: string
  modelCatalogId: string
}

export interface BootstrapSetupAiPayload {
  credentials: BootstrapCredentialInput[]
  bindingSelections: BootstrapBindingInput[]
}

export interface BootstrapSetupPayload {
  login: string
  displayName: string
  password: string
  locale: UiLocale
  aiSetup: BootstrapSetupAiPayload | null
}
