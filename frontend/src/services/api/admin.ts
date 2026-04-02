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
  AdminOpsLibrarySnapshot,
  AdminOpsLibraryState,
  AdminOpsLibraryWarning,
  AdminKnowledgeGeneration,
  AdminModelCatalogEntry,
  AdminPriceCatalogEntry,
  AdminWorkspaceMembership,
  CreateAdminPricePayload,
  CreateAdminCredentialPayload,
  CreateAdminModelPresetPayload,
  CreateApiTokenPayload,
  CreateApiTokenResult,
  SaveAdminLibraryBindingPayload,
  UpdateAdminPricePayload,
  UpdateAdminCredentialPayload,
  UpdateAdminModelPresetPayload,
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
    login: string
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
  allowedBindingPurposes: string[]
  contextWindow: number | null
  maxOutputTokens: number | null
}

interface RawPriceCatalogEntry {
  id: string
  modelCatalogId: string
  billingUnit: string
  priceVariantKey: string
  requestInputTokensMin: number | null
  requestInputTokensMax: number | null
  unitPrice: string | number
  currencyCode: string
  effectiveFrom: string
  effectiveTo: string | null
  catalogScope: string
  workspaceId: string | null
}

interface RawProviderCredential {
  id: string
  workspaceId: string
  providerCatalogId: string
  label: string
  apiKeySummary: string
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

interface RawOpsLibraryState {
  libraryId: string
  queueDepth: number
  runningAttempts: number
  readableDocumentCount: number
  failedDocumentCount: number
  degradedState: string
  latestKnowledgeGenerationId: string | null
  knowledgeGenerationState: string | null
  lastRecomputedAt: string
}

interface RawOpsLibraryStateWire {
  libraryId?: string
  queueDepth?: number
  runningAttempts?: number
  readableDocumentCount?: number
  failedDocumentCount?: number
  degradedState?: string
  latestKnowledgeGenerationId?: string | null
  knowledgeGenerationState?: string | null
  lastRecomputedAt?: string
  library_id?: string
  queue_depth?: number
  running_attempts?: number
  readable_document_count?: number
  failed_document_count?: number
  degraded_state?: string
  latest_knowledge_generation_id?: string | null
  knowledge_generation_state?: string | null
  last_recomputed_at?: string
}

interface RawKnowledgeGeneration {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  generationId: string
  workspaceId: string
  libraryId: string
  generationState: string
  activeTextGeneration: number
  activeVectorGeneration: number
  activeGraphGeneration: number
  degradedState: string
  createdAt: string
  updatedAt: string
}

interface RawKnowledgeGenerationWire {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  generationId?: string
  workspaceId?: string
  libraryId?: string
  generationState?: string
  activeTextGeneration?: number
  activeVectorGeneration?: number
  activeGraphGeneration?: number
  degradedState?: string
  createdAt?: string
  updatedAt?: string
  generation_id?: string
  workspace_id?: string
  library_id?: string
  generation_state?: string
  active_text_generation?: number
  active_vector_generation?: number
  active_graph_generation?: number
  degraded_state?: string
  created_at?: string
  updated_at?: string
}

interface RawOpsLibraryWarning {
  id: string
  libraryId: string
  warningKind: string
  severity: string
  createdAt: string
  resolvedAt: string | null
}

interface RawOpsLibraryWarningWire {
  id: string
  libraryId?: string
  warningKind?: string
  severity: string
  createdAt?: string
  resolvedAt?: string | null
  library_id?: string
  warning_kind?: string
  created_at?: string
  resolved_at?: string | null
}

interface RawOpsLibrarySnapshotWire {
  state: RawOpsLibraryStateWire
  knowledgeGenerations?: RawKnowledgeGenerationWire[]
  warnings: RawOpsLibraryWarningWire[]
  knowledge_generations?: RawKnowledgeGenerationWire[]
}

export function mapTokenRow(
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

export function mapGrant(row: RawGrantResponse): AdminGrant {
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

function mapWorkspaceMembership(row: RawWorkspaceMembershipResponse): AdminWorkspaceMembership {
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
    login: item.user?.login ?? null,
    email: item.user?.email ?? null,
    displayName: item.user?.displayName ?? null,
    authProviderKind: item.user?.authProviderKind ?? null,
    externalSubject: item.user?.externalSubject ?? null,
    workspaceMemberships: item.workspaceMemberships.map((row) => mapWorkspaceMembership(row)),
    effectiveGrants: item.effectiveGrants.map((row) => mapGrant(row)),
  }
}

export function mapProvider(row: RawProviderCatalogEntry): AdminProviderCatalogEntry {
  return {
    id: row.id,
    providerKind: row.providerKind,
    displayName: row.displayName,
    apiStyle: row.apiStyle,
    lifecycleState: row.lifecycleState,
  }
}

export function mapModel(row: RawModelCatalogEntry): AdminModelCatalogEntry {
  return {
    id: row.id,
    providerCatalogId: row.providerCatalogId,
    modelName: row.modelName,
    capabilityKind: row.capabilityKind,
    modalityKind: row.modalityKind,
    allowedBindingPurposes: row.allowedBindingPurposes,
    contextWindow: row.contextWindow,
    maxOutputTokens: row.maxOutputTokens,
  }
}

export function mapPrice(row: RawPriceCatalogEntry): AdminPriceCatalogEntry {
  return {
    id: row.id,
    modelCatalogId: row.modelCatalogId,
    billingUnit: row.billingUnit,
    priceVariantKey: row.priceVariantKey,
    requestInputTokensMin: row.requestInputTokensMin,
    requestInputTokensMax: row.requestInputTokensMax,
    unitPrice: String(row.unitPrice),
    currencyCode: row.currencyCode,
    effectiveFrom: row.effectiveFrom,
    effectiveTo: row.effectiveTo,
    workspaceId: row.workspaceId,
    setInWorkspace: row.catalogScope === 'workspace_override',
  }
}

export function mapCredential(row: RawProviderCredential): AdminProviderCredential {
  return {
    id: row.id,
    workspaceId: row.workspaceId,
    providerCatalogId: row.providerCatalogId,
    label: row.label,
    apiKeySummary: row.apiKeySummary,
    credentialState: row.credentialState,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  }
}

export function mapModelPreset(row: RawModelPreset): AdminModelPreset {
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

export function mapBinding(row: RawLibraryBinding): AdminLibraryBinding {
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

function mapAuditSubject(row: RawAuditEventSubject): AdminAuditEventSubject {
  return {
    auditEventId: row.auditEventId,
    subjectKind: row.subjectKind,
    subjectId: row.subjectId,
    workspaceId: row.workspaceId,
    libraryId: row.libraryId,
    documentId: row.documentId,
  }
}

export function mapAuditEvent(row: RawAuditEvent): AdminAuditEvent {
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

export function mapOpsLibraryState(
  row: RawOpsLibraryState | RawOpsLibraryStateWire,
): AdminOpsLibraryState {
  const libraryId = 'library_id' in row ? row.library_id : null
  const queueDepth = 'queue_depth' in row ? row.queue_depth : null
  const runningAttempts = 'running_attempts' in row ? row.running_attempts : null
  const readableDocumentCount =
    'readable_document_count' in row ? row.readable_document_count : null
  const failedDocumentCount = 'failed_document_count' in row ? row.failed_document_count : null
  const degradedState = 'degraded_state' in row ? row.degraded_state : null
  const latestKnowledgeGenerationId =
    'latest_knowledge_generation_id' in row ? row.latest_knowledge_generation_id : null
  const knowledgeGenerationState =
    'knowledge_generation_state' in row ? row.knowledge_generation_state : null
  const lastRecomputedAt = 'last_recomputed_at' in row ? row.last_recomputed_at : null
  return {
    libraryId: row.libraryId ?? libraryId ?? '',
    queueDepth: row.queueDepth ?? queueDepth ?? 0,
    runningAttempts: row.runningAttempts ?? runningAttempts ?? 0,
    readableDocumentCount: row.readableDocumentCount ?? readableDocumentCount ?? 0,
    failedDocumentCount: row.failedDocumentCount ?? failedDocumentCount ?? 0,
    degradedState: row.degradedState ?? degradedState ?? 'healthy',
    latestKnowledgeGenerationId:
      row.latestKnowledgeGenerationId ?? latestKnowledgeGenerationId ?? null,
    knowledgeGenerationState: row.knowledgeGenerationState ?? knowledgeGenerationState ?? null,
    lastRecomputedAt: row.lastRecomputedAt ?? lastRecomputedAt ?? '',
  }
}

function mapKnowledgeGeneration(
  row: RawKnowledgeGeneration | RawKnowledgeGenerationWire,
): AdminKnowledgeGeneration {
  const generationId = 'generation_id' in row ? row.generation_id : null
  const workspaceId = 'workspace_id' in row ? row.workspace_id : null
  const libraryId = 'library_id' in row ? row.library_id : null
  const generationState = 'generation_state' in row ? row.generation_state : null
  const activeTextGeneration = 'active_text_generation' in row ? row.active_text_generation : null
  const activeVectorGeneration =
    'active_vector_generation' in row ? row.active_vector_generation : null
  const activeGraphGeneration =
    'active_graph_generation' in row ? row.active_graph_generation : null
  const degradedState = 'degraded_state' in row ? row.degraded_state : null
  const createdAt = 'created_at' in row ? row.created_at : null
  const updatedAt = 'updated_at' in row ? row.updated_at : null
  return {
    key: row.key,
    generationId: row.generationId ?? generationId ?? '',
    workspaceId: row.workspaceId ?? workspaceId ?? '',
    libraryId: row.libraryId ?? libraryId ?? '',
    generationState: row.generationState ?? generationState ?? 'ready',
    activeTextGeneration: row.activeTextGeneration ?? activeTextGeneration ?? 0,
    activeVectorGeneration: row.activeVectorGeneration ?? activeVectorGeneration ?? 0,
    activeGraphGeneration: row.activeGraphGeneration ?? activeGraphGeneration ?? 0,
    degradedState: row.degradedState ?? degradedState ?? 'healthy',
    createdAt: row.createdAt ?? createdAt ?? '',
    updatedAt: row.updatedAt ?? updatedAt ?? '',
  }
}

function mapOpsLibraryWarning(
  row: RawOpsLibraryWarning | RawOpsLibraryWarningWire,
): AdminOpsLibraryWarning {
  const libraryId = 'library_id' in row ? row.library_id : null
  const warningKind = 'warning_kind' in row ? row.warning_kind : null
  const createdAt = 'created_at' in row ? row.created_at : null
  const resolvedAt = 'resolved_at' in row ? row.resolved_at : null
  return {
    id: row.id,
    libraryId: row.libraryId ?? libraryId ?? '',
    warningKind: row.warningKind ?? warningKind ?? 'warning',
    severity: row.severity,
    createdAt: row.createdAt ?? createdAt ?? '',
    resolvedAt: row.resolvedAt ?? resolvedAt ?? null,
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

export async function fetchAdminApiTokens(workspaceId: string | null): Promise<AdminApiTokenRow[]> {
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

export async function fetchAdminModelPresets(workspaceId: string): Promise<AdminModelPreset[]> {
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
        apiKey: payload.apiKey,
      }),
    ),
  )
}

export async function updateAdminCredential(
  payload: UpdateAdminCredentialPayload,
): Promise<AdminProviderCredential> {
  return mapCredential(
    await unwrap(
      apiHttp.put<RawProviderCredential>(`/ai/credentials/${payload.credentialId}`, {
        label: payload.label,
        apiKey: payload.apiKey,
        credentialState: payload.credentialState,
      }),
    ),
  )
}

export async function createAdminPrice(
  payload: CreateAdminPricePayload,
): Promise<AdminPriceCatalogEntry> {
  return mapPrice(
    await unwrap(
      apiHttp.post<RawPriceCatalogEntry>('/ai/prices', {
        workspaceId: payload.workspaceId,
        modelCatalogId: payload.modelCatalogId,
        billingUnit: payload.billingUnit,
        unitPrice: payload.unitPrice,
        currencyCode: payload.currencyCode,
        effectiveFrom: payload.effectiveFrom,
        effectiveTo: payload.effectiveTo,
      }),
    ),
  )
}

export async function updateAdminPrice(
  payload: UpdateAdminPricePayload,
): Promise<AdminPriceCatalogEntry> {
  return mapPrice(
    await unwrap(
      apiHttp.put<RawPriceCatalogEntry>(`/ai/prices/${payload.priceId}`, {
        modelCatalogId: payload.modelCatalogId,
        billingUnit: payload.billingUnit,
        unitPrice: payload.unitPrice,
        currencyCode: payload.currencyCode,
        effectiveFrom: payload.effectiveFrom,
        effectiveTo: payload.effectiveTo,
      }),
    ),
  )
}

export async function fetchAdminLibraryBindings(libraryId: string): Promise<AdminLibraryBinding[]> {
  return unwrap(apiHttp.get<RawLibraryBinding[]>(`/ai/libraries/${libraryId}/bindings`)).then(
    (rows) => rows.map((row) => mapBinding(row)),
  )
}

export async function createAdminModelPreset(
  payload: CreateAdminModelPresetPayload,
): Promise<AdminModelPreset> {
  return mapModelPreset(
    await unwrap(
      apiHttp.post<RawModelPreset>('/ai/model-presets', {
        workspaceId: payload.workspaceId,
        modelCatalogId: payload.modelCatalogId,
        presetName: payload.presetName,
        systemPrompt: payload.systemPrompt,
        temperature: payload.temperature,
        topP: payload.topP,
        maxOutputTokensOverride: payload.maxOutputTokensOverride,
        extraParametersJson: payload.extraParametersJson,
      }),
    ),
  )
}

export async function updateAdminModelPreset(
  payload: UpdateAdminModelPresetPayload,
): Promise<AdminModelPreset> {
  return mapModelPreset(
    await unwrap(
      apiHttp.put<RawModelPreset>(`/ai/model-presets/${payload.presetId}`, {
        presetName: payload.presetName,
        systemPrompt: payload.systemPrompt,
        temperature: payload.temperature,
        topP: payload.topP,
        maxOutputTokensOverride: payload.maxOutputTokensOverride,
        extraParametersJson: payload.extraParametersJson,
      }),
    ),
  )
}

export async function saveAdminLibraryBinding(
  payload: SaveAdminLibraryBindingPayload,
): Promise<AdminLibraryBinding> {
  if (payload.bindingId) {
    return mapBinding(
      await unwrap(
        apiHttp.put<RawLibraryBinding>(`/ai/library-bindings/${payload.bindingId}`, {
          providerCredentialId: payload.providerCredentialId,
          modelPresetId: payload.modelPresetId,
          bindingState: payload.bindingState,
        }),
      ),
    )
  }

  return mapBinding(
    await unwrap(
      apiHttp.post<RawLibraryBinding>('/ai/library-bindings', {
        workspaceId: payload.workspaceId,
        libraryId: payload.libraryId,
        bindingPurpose: payload.bindingPurpose,
        providerCredentialId: payload.providerCredentialId,
        modelPresetId: payload.modelPresetId,
      }),
    ),
  )
}

export async function validateAdminLibraryBinding(
  bindingId: string,
): Promise<AdminBindingValidation> {
  return mapBindingValidation(
    await unwrap(apiHttp.post<RawBindingValidation>(`/ai/library-bindings/${bindingId}/validate`)),
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

export async function fetchAdminLibraryOpsState(
  libraryId: string,
): Promise<AdminOpsLibrarySnapshot> {
  const payload = await unwrap(
    apiHttp.get<RawOpsLibrarySnapshotWire>(`/ops/libraries/${libraryId}`),
  )
  return {
    state: mapOpsLibraryState(payload.state),
    knowledgeGenerations: (payload.knowledgeGenerations ?? payload.knowledge_generations ?? []).map(
      (row) => mapKnowledgeGeneration(row),
    ),
    warnings: payload.warnings.map((row) => mapOpsLibraryWarning(row)),
  }
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
