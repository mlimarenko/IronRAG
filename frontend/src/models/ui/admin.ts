export type AdminTab = 'api_tokens' | 'members' | 'library_access' | 'settings'

export interface AdminOverviewResponse {
  activeTab: AdminTab
  workspaceName: string
  counts: {
    apiTokens: number
    members: number
    libraryAccess: number
    settings: number
  }
  availability: {
    apiTokens: boolean
    members: boolean
    libraryAccess: boolean
    settings: boolean
  }
}

export interface ApiTokenRow {
  id: string
  label: string
  maskedToken: string
  scopes: string[]
  createdAt: string
  lastUsedAt: string | null
  expiresAt: string | null
  canRevoke: boolean
  plaintextToken: string | null
}

export interface CreateApiTokenPayload {
  label: string
  scopes: string[]
  expiresInDays: number | null
}

export interface CreateApiTokenResult {
  row: ApiTokenRow
  plaintextToken: string
}

export interface AdminMemberRow {
  id: string
  displayName: string
  email: string
  roleLabel: string
}

export interface LibraryAccessRow {
  id: string
  libraryName: string
  principalLabel: string
  accessLevel: string
}

export interface AdminSettingItem {
  id: string
  label: string
  value: string
}

export interface AdminSupportedProvider {
  providerKind: string
  supportedCapabilities: string[]
  defaultModels: Partial<Record<string, string>>
  availableModels: Partial<Record<string, string[]>>
  isConfigured: boolean
}

export interface AdminProviderProfile {
  libraryId: string
  libraryName: string
  indexingProviderKind: string
  indexingModelName: string
  embeddingProviderKind: string
  embeddingModelName: string
  answerProviderKind: string
  answerModelName: string
  visionProviderKind: string
  visionModelName: string
  lastValidatedAt: string | null
  lastValidationStatus: string | null
  lastValidationError: string | null
}

export interface AdminProviderValidationCheck {
  providerKind: string
  modelName: string
  capability: string
  status: string
  checkedAt: string
  error: string | null
}

export interface AdminProviderValidation {
  status: string | null
  checkedAt: string | null
  error: string | null
  checks: AdminProviderValidationCheck[]
}

export interface AdminPricingCatalogEntry {
  id: string
  workspaceId: string | null
  providerKind: string
  modelName: string
  capability: string
  billingUnit: string
  inputPrice: string | null
  outputPrice: string | null
  currency: string
  status: string
  sourceKind: string
  note: string | null
  effectiveFrom: string
  effectiveTo: string | null
}

export interface AdminPricingCoverageWarning {
  providerKind: string
  modelName: string
  capability: string
  billingUnit: string
  message: string
}

export interface AdminPricingCoverageSummary {
  status: 'covered' | 'partial' | 'missing'
  coveredTargets: number
  missingTargets: number
  warnings: AdminPricingCoverageWarning[]
}

export interface AdminUpsertPricingEntryPayload {
  workspaceId: string | null
  providerKind: string
  modelName: string
  capability: string
  billingUnit: string
  inputPrice: number | null
  outputPrice: number | null
  currency: string
  note: string | null
  effectiveFrom: string
}

export interface AdminSettingsResponse {
  items: AdminSettingItem[]
  providerCatalog: AdminSupportedProvider[]
  providerProfile: AdminProviderProfile
  providerValidation: AdminProviderValidation
  pricingCatalog: AdminPricingCatalogEntry[]
  pricingCoverage: AdminPricingCoverageSummary
  liveValidationEnabled: boolean
  supportedProviderKinds: string[]
}

export interface UpdateAdminProviderProfilePayload {
  indexingProviderKind: string
  indexingModelName: string
  embeddingProviderKind: string
  embeddingModelName: string
  answerProviderKind: string
  answerModelName: string
  visionProviderKind: string
  visionModelName: string
}
