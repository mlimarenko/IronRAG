import type {
  AdminAiConsoleState,
  AdminApiTokenRow,
  AdminAuditEvent,
  AdminAuditEventSubject,
  AdminBindingValidation,
  AdminGrant,
  AdminGrantResourceKind,
  AdminModelPreset,
  AdminPermissionKind,
  AdminPrincipalSummary,
  AdminProviderCatalogEntry,
  AdminProviderCredential,
  AdminLibraryBinding,
  AdminModelCatalogEntry,
  AdminPriceCatalogEntry,
  AdminWorkspaceMembership,
  CreateAdminCredentialPayload,
  CreateApiTokenPayload,
  CreateApiTokenResult,
} from 'src/models/ui/admin'
import { apiHttp, unwrap } from './http'

interface RawTokenResponse {
  principalId: string
  workspaceId: string | null
  label: string
  tokenPrefix: string
  status: string
  expiresAt: string | null
  revokedAt: string | null
  issuedByPrincipalId: string | null
  lastUsedAt: string | null
}

interface RawMintTokenResponse {
  token: string
  apiToken: RawTokenResponse
}

interface RawGrantResponse {
  id: string
  principalId: string
  resourceKind: AdminGrantResourceKind
  resourceId: string
  permissionKind: AdminPermissionKind
  grantedByPrincipalId: string | null
  grantedAt: string
  expiresAt: string | null
}

interface RawModelPreset {
  id: string
  workspaceId: string
  modelCatalogId: string
  presetName: string
  systemPrompt: string | null
  temperature: number | null
  topP: number | null
  maxOutputTokensOverride: number | null
  extraParametersJson: unknown
  createdAt: string
  updatedAt: string
}

interface RawWorkspaceMembershipResponse {
  workspaceId: string
  principalId: string
  membershipState: string
  joinedAt: string
  endedAt: string | null
}

interface RawMeResponse {
  principal: {
    id: string
    principalKind: string
    status: string
    displayLabel: string
    createdAt: string
    disabledAt: string | null
  }
  user: {
    principalId: string
    email: string
    displayName: string
    authProviderKind: string
    externalSubject: string | null
  } | null
  workspaceMemberships: RawWorkspaceMembershipResponse[]
  effectiveGrants: RawGrantResponse[]
}

interface RawProviderCatalogEntry {
  id: string
  providerKind: string
  displayName: string
  apiStyle: string
  lifecycleState: string
}

interface RawModelCatalogEntry {
  id: string
  providerCatalogId: string
  modelName: string
  capabilityKind: string
  modalityKind: string
  contextWindow: number | null
  maxOutputTokens: number | null
}

interface RawPriceCatalogEntry {
  id: string
  modelCatalogId: string
  billingUnit: string
  unitPrice: string | number
  currencyCode: string
  effectiveFrom: string
}

interface RawProviderCredential {
  id: string
  workspaceId: string
  providerCatalogId: string
  label: string
  secretRef: string
  credentialState: string
  createdAt: string
  updatedAt: string
}

interface RawLibraryBinding {
  id: string
  workspaceId: string
  libraryId: string
  bindingPurpose: string
  providerCredentialId: string
  modelPresetId: string
  bindingState: string
}

interface RawBindingValidation {
  id: string
  bindingId: string
  validationState: string
  checkedAt: string
  failureCode: string | null
  message: string | null
}

interface RawAuditEventSubject {
  auditEventId: string
  subjectKind: string
  subjectId: string
  workspaceId: string | null
  libraryId: string | null
  documentId: string | null
}

interface RawAuditEvent {
  id: string
  actorPrincipalId: string | null
  surfaceKind: string
  actionKind: string
  resultKind: string
  requestId: string | null
  traceId: string | null
  createdAt: string
  redactedMessage: string | null
  internalMessage?: string | null
  subjects: RawAuditEventSubject[]
}

function mapTokenRow(
  row: RawTokenResponse,
  plaintextToken: string | null = null,
  grants: AdminGrant[] = [],
): AdminApiTokenRow {
  return {
    principalId: row.principalId,
    workspaceId: row.workspaceId,
    label: row.label,
    tokenPrefix: row.tokenPrefix,
    status: row.status,
    expiresAt: row.expiresAt,
    revokedAt: row.revokedAt,
    issuedByPrincipalId: row.issuedByPrincipalId,
    lastUsedAt: row.lastUsedAt,
    plaintextToken,
    grants,
  }
}

function mapGrant(row: RawGrantResponse): AdminGrant {
  return {
    id: row.id,
    principalId: row.principalId,
    resourceKind: row.resourceKind,
    resourceId: row.resourceId,
    permissionKind: row.permissionKind,
    grantedByPrincipalId: row.grantedByPrincipalId,
    grantedAt: row.grantedAt,
    expiresAt: row.expiresAt,
  }
}

function mapWorkspaceMembership(
  row: RawWorkspaceMembershipResponse,
): AdminWorkspaceMembership {
  return {
    workspaceId: row.workspaceId,
    principalId: row.principalId,
    membershipState: row.membershipState,
    joinedAt: row.joinedAt,
    endedAt: row.endedAt,
  }
}

function mapPrincipal(item: RawMeResponse): AdminPrincipalSummary {
  return {
    id: item.principal.id,
    principalKind: item.principal.principalKind,
    status: item.principal.status,
    displayLabel: item.principal.displayLabel,
    email: item.user?.email ?? null,
    displayName: item.user?.displayName ?? null,
    authProviderKind: item.user?.authProviderKind ?? null,
    externalSubject: item.user?.externalSubject ?? null,
    workspaceMemberships: item.workspaceMemberships.map((row) =>
      mapWorkspaceMembership(row),
    ),
    effectiveGrants: item.effectiveGrants.map((row) => mapGrant(row)),
  }
}

function mapProvider(row: RawProviderCatalogEntry): AdminProviderCatalogEntry {
  return {
    id: row.id,
    providerKind: row.providerKind,
    displayName: row.displayName,
    apiStyle: row.apiStyle,
    lifecycleState: row.lifecycleState,
  }
}

function mapModel(row: RawModelCatalogEntry): AdminModelCatalogEntry {
  return {
    id: row.id,
    providerCatalogId: row.providerCatalogId,
    modelName: row.modelName,
    capabilityKind: row.capabilityKind,
    modalityKind: row.modalityKind,
    contextWindow: row.contextWindow,
    maxOutputTokens: row.maxOutputTokens,
  }
}

function mapPrice(row: RawPriceCatalogEntry): AdminPriceCatalogEntry {
  return {
    id: row.id,
    modelCatalogId: row.modelCatalogId,
    billingUnit: row.billingUnit,
    unitPrice: String(row.unitPrice),
    currencyCode: row.currencyCode,
    effectiveFrom: row.effectiveFrom,
  }
}

function mapCredential(row: RawProviderCredential): AdminProviderCredential {
  return {
    id: row.id,
    workspaceId: row.workspaceId,
    providerCatalogId: row.providerCatalogId,
    label: row.label,
    secretRef: row.secretRef,
    credentialState: row.credentialState,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  }
}

function mapModelPreset(row: RawModelPreset): AdminModelPreset {
  return {
    id: row.id,
    workspaceId: row.workspaceId,
    modelCatalogId: row.modelCatalogId,
    presetName: row.presetName,
    systemPrompt: row.systemPrompt,
    temperature: row.temperature,
    topP: row.topP,
    maxOutputTokensOverride: row.maxOutputTokensOverride,
    extraParametersJson: row.extraParametersJson,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  }
}

function mapBindingValidation(row: RawBindingValidation): AdminBindingValidation {
  return {
    id: row.id,
    bindingId: row.bindingId,
    validationState: row.validationState,
    checkedAt: row.checkedAt,
    failureCode: row.failureCode,
    message: row.message,
  }
}

function mapBinding(row: RawLibraryBinding): AdminLibraryBinding {
  return {
    id: row.id,
    workspaceId: row.workspaceId,
    libraryId: row.libraryId,
    bindingPurpose: row.bindingPurpose,
    providerCredentialId: row.providerCredentialId,
    modelPresetId: row.modelPresetId,
    bindingState: row.bindingState,
    latestValidation: null,
  }
}

function mapAuditSubject(
  row: RawAuditEventSubject,
): AdminAuditEventSubject {
  return {
    auditEventId: row.auditEventId,
    subjectKind: row.subjectKind,
    subjectId: row.subjectId,
    workspaceId: row.workspaceId,
    libraryId: row.libraryId,
    documentId: row.documentId,
  }
}

function mapAuditEvent(row: RawAuditEvent): AdminAuditEvent {
  return {
    id: row.id,
    actorPrincipalId: row.actorPrincipalId,
    surfaceKind: row.surfaceKind,
    actionKind: row.actionKind,
    resultKind: row.resultKind,
    requestId: row.requestId,
    traceId: row.traceId,
    createdAt: row.createdAt,
    redactedMessage: row.redactedMessage,
    internalMessage: row.internalMessage ?? null,
    subjects: row.subjects.map((subject) => mapAuditSubject(subject)),
  }
}

function toExpiresAt(expiresInDays: number | null): string | null {
  if (expiresInDays === null) {
    return null
  }
  return new Date(Date.now() + expiresInDays * 24 * 60 * 60 * 1000).toISOString()
}

export async function fetchAdminPrincipal(): Promise<AdminPrincipalSummary> {
  return mapPrincipal(await unwrap(apiHttp.get<RawMeResponse>('/iam/me')))
}

export async function fetchAdminApiTokens(
  workspaceId: string | null,
): Promise<AdminApiTokenRow[]> {
  const tokens = await unwrap(
    apiHttp.get<RawTokenResponse[]>('/iam/tokens', {
      params: workspaceId ? { workspaceId } : {},
    }),
  )

  const grantRows = await Promise.all(
    tokens.map(async (row) => ({
      principalId: row.principalId,
      grants: await unwrap(
        apiHttp.get<RawGrantResponse[]>('/iam/grants', {
          params: { principalId: row.principalId },
        }),
      ),
    })),
  )

  const grantsByPrincipal = new Map(
    grantRows.map((entry) => [entry.principalId, entry.grants.map((row) => mapGrant(row))]),
  )

  return tokens.map((row) => mapTokenRow(row, null, grantsByPrincipal.get(row.principalId) ?? []))
}

export async function createAdminApiToken(
  payload: CreateApiTokenPayload,
): Promise<CreateApiTokenResult> {
  const response = await unwrap(
    apiHttp.post<RawMintTokenResponse>('/iam/tokens', {
      workspaceId: payload.workspaceId,
      label: payload.label,
      expiresAt: toExpiresAt(payload.expiresInDays),
    }),
  )

  return {
    row: mapTokenRow(response.apiToken, response.token),
    plaintextToken: response.token,
  }
}

export async function createAdminGrant(payload: {
  principalId: string
  resourceKind: Extract<AdminGrantResourceKind, 'workspace' | 'library'>
  resourceId: string
  permissionKind: AdminPermissionKind
}): Promise<AdminGrant> {
  return mapGrant(
    await unwrap(
      apiHttp.post<RawGrantResponse>('/iam/grants', {
        principalId: payload.principalId,
        resourceKind: payload.resourceKind,
        resourceId: payload.resourceId,
        permissionKind: payload.permissionKind,
      }),
    ),
  )
}

export async function fetchAdminModelPresets(
  workspaceId: string,
): Promise<AdminModelPreset[]> {
  return unwrap(
    apiHttp.get<RawModelPreset[]>('/ai/model-presets', {
      params: { workspaceId },
    }),
  ).then((rows) => rows.map((row) => mapModelPreset(row)))
}

export async function revokeAdminApiToken(principalId: string): Promise<void> {
  await unwrap(apiHttp.post(`/iam/tokens/${principalId}/revoke`))
}

export async function fetchAdminProviders(): Promise<AdminProviderCatalogEntry[]> {
  return unwrap(apiHttp.get<RawProviderCatalogEntry[]>('/ai/providers')).then((rows) =>
    rows.map((row) => mapProvider(row)),
  )
}

export async function fetchAdminModels(): Promise<AdminModelCatalogEntry[]> {
  return unwrap(apiHttp.get<RawModelCatalogEntry[]>('/ai/models')).then((rows) =>
    rows.map((row) => mapModel(row)),
  )
}

export async function fetchAdminPrices(
  workspaceId: string | null,
): Promise<AdminPriceCatalogEntry[]> {
  return unwrap(
    apiHttp.get<RawPriceCatalogEntry[]>('/ai/prices', {
      params: workspaceId ? { workspaceId } : {},
    }),
  ).then((rows) => rows.map((row) => mapPrice(row)))
}

export async function fetchAdminCredentials(
  workspaceId: string,
): Promise<AdminProviderCredential[]> {
  return unwrap(
    apiHttp.get<RawProviderCredential[]>('/ai/credentials', {
      params: { workspaceId },
    }),
  ).then((rows) => rows.map((row) => mapCredential(row)))
}

export async function createAdminCredential(
  payload: CreateAdminCredentialPayload,
): Promise<AdminProviderCredential> {
  return mapCredential(
    await unwrap(
      apiHttp.post<RawProviderCredential>('/ai/credentials', {
        workspaceId: payload.workspaceId,
        providerCatalogId: payload.providerCatalogId,
        label: payload.label,
        secretRef: payload.secretRef,
      }),
    ),
  )
}

export async function fetchAdminLibraryBindings(
  libraryId: string,
): Promise<AdminLibraryBinding[]> {
  return unwrap(
    apiHttp.get<RawLibraryBinding[]>(`/ai/library-bindings/${libraryId}`),
  ).then((rows) => rows.map((row) => mapBinding(row)))
}

export async function validateAdminLibraryBinding(
  bindingId: string,
): Promise<AdminBindingValidation> {
  return mapBindingValidation(
    await unwrap(
      apiHttp.post<RawBindingValidation>(
        `/ai/library-bindings/${bindingId}/validate`,
      ),
    ),
  )
}

export async function fetchAdminAuditEvents(params: {
  workspaceId?: string
  libraryId?: string
}): Promise<AdminAuditEvent[]> {
  return unwrap(
    apiHttp.get<RawAuditEvent[]>('/audit/events', {
      params,
    }),
  ).then((rows) => rows.map((row) => mapAuditEvent(row)))
}

export async function fetchAdminAiConsole(payload: {
  workspaceId: string
  workspaceName: string
  libraryId: string
  libraryName: string
}): Promise<AdminAiConsoleState> {
  const [providers, models, modelPresets, prices, credentials, bindings] = await Promise.all([
    fetchAdminProviders(),
    fetchAdminModels(),
    fetchAdminModelPresets(payload.workspaceId),
    fetchAdminPrices(payload.workspaceId),
    fetchAdminCredentials(payload.workspaceId),
    fetchAdminLibraryBindings(payload.libraryId),
  ])

  return {
    workspaceId: payload.workspaceId,
    workspaceName: payload.workspaceName,
    libraryId: payload.libraryId,
    libraryName: payload.libraryName,
    providers,
    models,
    modelPresets,
    prices,
    credentials,
    bindings,
  }
}
