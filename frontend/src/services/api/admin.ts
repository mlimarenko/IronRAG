import type {
  AdminMemberRow,
  AdminOverviewResponse,
  AdminPricingCatalogEntry,
  AdminPricingCoverageSummary,
  AdminProviderProfile,
  AdminProviderValidation,
  AdminProviderValidationCheck,
  AdminSettingsResponse,
  AdminSettingItem,
  AdminSupportedProvider,
  AdminUpsertPricingEntryPayload,
  ApiTokenRow,
  CreateApiTokenPayload,
  CreateApiTokenResult,
  LibraryAccessRow,
  UpdateAdminProviderProfilePayload,
} from 'src/models/ui/admin'
import { apiHttp, unwrap } from './http'

interface RawAdminOverviewResponse {
  active_tab: 'api_tokens' | 'members' | 'library_access' | 'settings'
  workspace_name: string
  counts: {
    api_tokens: number
    members: number
    library_access: number
    settings: number
  }
  availability: {
    api_tokens: boolean
    members: boolean
    library_access: boolean
    settings: boolean
  }
}

interface RawApiTokenRow {
  id: string
  label: string
  masked_token: string
  scopes: string[]
  created_at: string
  last_used_at: string | null
  expires_at: string | null
  can_revoke: boolean
}

interface RawApiTokensResponse {
  rows: RawApiTokenRow[]
}

interface RawCreateApiTokenResult {
  row: RawApiTokenRow
  plaintext_token: string
}

interface RawMembersResponse {
  rows: {
    id: string
    display_name: string
    email: string
    role_label: string
  }[]
}

interface RawLibraryAccessResponse {
  rows: {
    id: string
    library_name: string
    principal_label: string
    access_level: string
  }[]
}

interface RawSettingsResponse {
  items: {
    id: string
    label: string
    value: string
  }[]
  provider_catalog: {
    provider_kind: string
    supported_capabilities: string[]
    default_models: Record<string, string>
    available_models: Record<string, string[]>
    is_configured: boolean
  }[]
  provider_profile: {
    library_id: string
    library_name: string
    indexing_provider_kind: string
    indexing_model_name: string
    embedding_provider_kind: string
    embedding_model_name: string
    answer_provider_kind: string
    answer_model_name: string
    vision_provider_kind: string
    vision_model_name: string
    last_validated_at: string | null
    last_validation_status: string | null
    last_validation_error: string | null
  }
  provider_validation: {
    status: string | null
    checked_at: string | null
    error: string | null
    checks: {
      provider_kind: string
      model_name: string
      capability: string
      status: string
      checked_at: string
      error: string | null
    }[]
  }
  pricing_catalog: {
    id: string
    workspace_id: string | null
    provider_kind: string
    model_name: string
    capability: string
    billing_unit: string
    input_price: string | null
    output_price: string | null
    currency: string
    status: string
    source_kind: string
    note: string | null
    effective_from: string
    effective_to: string | null
  }[]
  pricing_coverage: {
    status: 'covered' | 'partial' | 'missing'
    covered_targets: number
    missing_targets: number
    warnings: {
      provider_kind: string
      model_name: string
      capability: string
      billing_unit: string
      message: string
    }[]
  }
  live_validation_enabled: boolean
  supported_provider_kinds: string[]
}

interface RawProviderProfileResponse {
  profile: RawSettingsResponse['provider_profile']
}

interface RawProviderValidationResponse {
  profile: RawSettingsResponse['provider_profile']
  validation: RawSettingsResponse['provider_validation']
}

interface RawPricingCatalogEntry {
  id: string
  workspace_id: string | null
  provider_kind: string
  model_name: string
  capability: string
  billing_unit: string
  input_price: string | null
  output_price: string | null
  currency: string
  status: string
  source_kind: string
  note: string | null
  effective_from: string
  effective_to: string | null
}

function mapTokenRow(row: RawApiTokenRow, plaintextToken: string | null = null): ApiTokenRow {
  return {
    id: row.id,
    label: row.label,
    maskedToken: row.masked_token,
    scopes: row.scopes,
    createdAt: row.created_at,
    lastUsedAt: row.last_used_at,
    expiresAt: row.expires_at,
    canRevoke: row.can_revoke,
    plaintextToken,
  }
}

function mapSettingItem(item: RawSettingsResponse['items'][number]): AdminSettingItem {
  return {
    id: item.id,
    label: item.label,
    value: item.value,
  }
}

function mapSupportedProvider(
  provider: RawSettingsResponse['provider_catalog'][number],
): AdminSupportedProvider {
  return {
    providerKind: provider.provider_kind,
    supportedCapabilities: provider.supported_capabilities,
    defaultModels: provider.default_models,
    availableModels: provider.available_models,
    isConfigured: provider.is_configured,
  }
}

function mapProviderProfile(
  profile: RawSettingsResponse['provider_profile'],
): AdminProviderProfile {
  return {
    libraryId: profile.library_id,
    libraryName: profile.library_name,
    indexingProviderKind: profile.indexing_provider_kind,
    indexingModelName: profile.indexing_model_name,
    embeddingProviderKind: profile.embedding_provider_kind,
    embeddingModelName: profile.embedding_model_name,
    answerProviderKind: profile.answer_provider_kind,
    answerModelName: profile.answer_model_name,
    visionProviderKind: profile.vision_provider_kind,
    visionModelName: profile.vision_model_name,
    lastValidatedAt: profile.last_validated_at,
    lastValidationStatus: profile.last_validation_status,
    lastValidationError: profile.last_validation_error,
  }
}

function mapProviderValidationCheck(
  check: RawSettingsResponse['provider_validation']['checks'][number],
): AdminProviderValidationCheck {
  return {
    providerKind: check.provider_kind,
    modelName: check.model_name,
    capability: check.capability,
    status: check.status,
    checkedAt: check.checked_at,
    error: check.error,
  }
}

function mapProviderValidation(
  validation: RawSettingsResponse['provider_validation'],
): AdminProviderValidation {
  return {
    status: validation.status,
    checkedAt: validation.checked_at,
    error: validation.error,
    checks: validation.checks.map((check) => mapProviderValidationCheck(check)),
  }
}

function mapPricingCatalogEntry(
  row: RawPricingCatalogEntry,
): AdminPricingCatalogEntry {
  return {
    id: row.id,
    workspaceId: row.workspace_id,
    providerKind: row.provider_kind,
    modelName: row.model_name,
    capability: row.capability,
    billingUnit: row.billing_unit,
    inputPrice: row.input_price,
    outputPrice: row.output_price,
    currency: row.currency,
    status: row.status,
    sourceKind: row.source_kind,
    note: row.note,
    effectiveFrom: row.effective_from,
    effectiveTo: row.effective_to,
  }
}

function mapPricingCoverage(
  coverage: RawSettingsResponse['pricing_coverage'],
): AdminPricingCoverageSummary {
  return {
    status: coverage.status,
    coveredTargets: coverage.covered_targets,
    missingTargets: coverage.missing_targets,
    warnings: coverage.warnings.map((warning) => ({
      providerKind: warning.provider_kind,
      modelName: warning.model_name,
      capability: warning.capability,
      billingUnit: warning.billing_unit,
      message: warning.message,
    })),
  }
}

export async function fetchAdminOverview(): Promise<AdminOverviewResponse> {
  const response = await unwrap(apiHttp.get<RawAdminOverviewResponse>('/ui/admin/overview'))
  return {
    activeTab: response.active_tab,
    workspaceName: response.workspace_name,
    counts: {
      apiTokens: response.counts.api_tokens,
      members: response.counts.members,
      libraryAccess: response.counts.library_access,
      settings: response.counts.settings,
    },
    availability: {
      apiTokens: response.availability.api_tokens,
      members: response.availability.members,
      libraryAccess: response.availability.library_access,
      settings: response.availability.settings,
    },
  }
}

export async function fetchAdminApiTokens(): Promise<ApiTokenRow[]> {
  const response = await unwrap(apiHttp.get<RawApiTokensResponse>('/ui/admin/api-tokens'))
  return response.rows.map((row) => mapTokenRow(row))
}

export async function createAdminApiToken(
  payload: CreateApiTokenPayload,
): Promise<CreateApiTokenResult> {
  const response = await unwrap(
    apiHttp.post<RawCreateApiTokenResult>('/ui/admin/api-tokens', {
      label: payload.label,
      scopes: payload.scopes,
      expires_in_days: payload.expiresInDays,
    }),
  )

  return {
    row: mapTokenRow(response.row, response.plaintext_token),
    plaintextToken: response.plaintext_token,
  }
}

export async function revokeAdminApiToken(id: string): Promise<ApiTokenRow> {
  return mapTokenRow(await unwrap(apiHttp.delete<RawApiTokenRow>(`/ui/admin/api-tokens/${id}`)))
}

export async function fetchAdminMembers(): Promise<AdminMemberRow[]> {
  const response = await unwrap(apiHttp.get<RawMembersResponse>('/ui/admin/members'))
  return response.rows.map((row) => ({
    id: row.id,
    displayName: row.display_name,
    email: row.email,
    roleLabel: row.role_label,
  }))
}

export async function fetchAdminLibraryAccess(): Promise<LibraryAccessRow[]> {
  const response = await unwrap(
    apiHttp.get<RawLibraryAccessResponse>('/ui/admin/library-access'),
  )
  return response.rows.map((row) => ({
    id: row.id,
    libraryName: row.library_name,
    principalLabel: row.principal_label,
    accessLevel: row.access_level,
  }))
}

export async function fetchAdminSettings(): Promise<AdminSettingsResponse> {
  const response = await unwrap(apiHttp.get<RawSettingsResponse>('/ui/admin/settings'))
  return {
    items: response.items.map((item) => mapSettingItem(item)),
    providerCatalog: response.provider_catalog.map((provider) => mapSupportedProvider(provider)),
    providerProfile: mapProviderProfile(response.provider_profile),
    providerValidation: mapProviderValidation(response.provider_validation),
    pricingCatalog: response.pricing_catalog.map((row) => mapPricingCatalogEntry(row)),
    pricingCoverage: mapPricingCoverage(response.pricing_coverage),
    liveValidationEnabled: response.live_validation_enabled,
    supportedProviderKinds: response.supported_provider_kinds,
  }
}

export async function updateAdminProviderProfile(
  payload: UpdateAdminProviderProfilePayload,
): Promise<AdminProviderProfile> {
  const response = await unwrap(
    apiHttp.put<RawProviderProfileResponse>('/ui/admin/settings/provider-profile', {
      indexingProviderKind: payload.indexingProviderKind,
      indexingModelName: payload.indexingModelName,
      embeddingProviderKind: payload.embeddingProviderKind,
      embeddingModelName: payload.embeddingModelName,
      answerProviderKind: payload.answerProviderKind,
      answerModelName: payload.answerModelName,
      visionProviderKind: payload.visionProviderKind,
      visionModelName: payload.visionModelName,
    }),
  )

  return mapProviderProfile(response.profile)
}

export async function validateAdminProviderProfile(): Promise<{
  profile: AdminProviderProfile
  validation: AdminProviderValidation
}> {
  const response = await unwrap(
    apiHttp.post<RawProviderValidationResponse>('/ui/admin/settings/provider-profile/validate'),
  )

  return {
    profile: mapProviderProfile(response.profile),
    validation: mapProviderValidation(response.validation),
  }
}

export async function createAdminPricingEntry(
  payload: AdminUpsertPricingEntryPayload,
): Promise<AdminPricingCatalogEntry> {
  return mapPricingCatalogEntry(
    await unwrap(
      apiHttp.post<RawPricingCatalogEntry>('/runtime/admin/model-pricing', {
        workspaceId: payload.workspaceId,
        providerKind: payload.providerKind,
        modelName: payload.modelName,
        capability: payload.capability,
        billingUnit: payload.billingUnit,
        inputPrice: payload.inputPrice,
        outputPrice: payload.outputPrice,
        currency: payload.currency,
        note: payload.note,
        effectiveFrom: payload.effectiveFrom,
      }),
    ),
  )
}

export async function updateAdminPricingEntry(
  pricingId: string,
  payload: AdminUpsertPricingEntryPayload,
): Promise<AdminPricingCatalogEntry> {
  return mapPricingCatalogEntry(
    await unwrap(
      apiHttp.put<RawPricingCatalogEntry>(`/runtime/admin/model-pricing/${pricingId}`, {
        workspaceId: payload.workspaceId,
        providerKind: payload.providerKind,
        modelName: payload.modelName,
        capability: payload.capability,
        billingUnit: payload.billingUnit,
        inputPrice: payload.inputPrice,
        outputPrice: payload.outputPrice,
        currency: payload.currency,
        note: payload.note,
        effectiveFrom: payload.effectiveFrom,
      }),
    ),
  )
}

export async function deactivateAdminPricingEntry(
  pricingId: string,
): Promise<AdminPricingCatalogEntry> {
  return mapPricingCatalogEntry(
    await unwrap(apiHttp.delete<RawPricingCatalogEntry>(`/runtime/admin/model-pricing/${pricingId}`)),
  )
}
