import { Admin, Ai, Audit, Catalog, Iam, Ops } from './generated'
import { unwrap } from './runtime'
import {
  resolveProviderBaseUrlPolicy,
  resolveProviderCredentialPolicy,
  resolveProviderModelDiscovery,
} from '@/shared/lib/ai-provider'
import type {
  CreateAccountRequest,
  UpdateAccountRequest,
  CreateBindingRequest,
  UpdateBindingRequest,
  CreateProviderRequest,
  UpdateProviderRequest,
  CreateModelRequest,
  UpdateModelRequest,
  CreatePriceOverrideRequest,
  UpdatePriceOverrideRequest,
} from '@/shared/types/api-requests'
import type {
  AdminSurface as AdminSurfaceResponse,
  AiAccountResponse,
  AiBindingResponse,
  BindingValidationResponse,
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  CreateAiAccountRequest as GeneratedCreateAccountRequest,
  CreateAiBindingRequest as GeneratedCreateBindingRequest,
  BulkIngestQueueActionResponse,
  CreateModelCatalogRequest as GeneratedCreateModelRequest,
  CreateProviderCatalogRequest as GeneratedCreateProviderRequest,
  CreateWorkspacePriceOverrideRequest as GeneratedCreatePriceOverrideRequest,
  IngestQueueMoveDirection,
  IngestQueueBulkAction,
  IngestQueueResponse,
  ModelAvailabilityState,
  ModelCatalogEntryResponse,
  PriceCatalogEntryResponse,
  ProviderCatalogEntryResponse,
  UpdateAiAccountRequest as GeneratedUpdateAccountRequest,
  UpdateAiBindingRequest as GeneratedUpdateBindingRequest,
  UpdateModelCatalogRequest as GeneratedUpdateModelRequest,
  UpdateProviderCatalogRequest as GeneratedUpdateProviderRequest,
  UpdateWorkspacePriceOverrideRequest as GeneratedUpdatePriceOverrideRequest,
  ListAiAccountsData,
  ListAiModelsData,
  ListAiPricesData,
  ListAuditEventsData,
  ListIngestQueueData,
  UpdateLibraryRecognitionPolicyRequest,
  UpdateLibraryWebIngestPolicyRequest,
  MintTokenRequest as GeneratedMintTokenRequest,
  MintTokenResponse,
  TokenResponse,
  AuditEventPageResponse,
  CreateUserRequest as GeneratedCreateUserRequest,
  SetUserAccessRequest as GeneratedSetUserAccessRequest,
  SystemRole,
  UserAccessResponse,
  UserResponse,
} from './generated'

type ListAuditEventsParams = NonNullable<ListAuditEventsData['query']>
type ListIngestQueueParams = NonNullable<ListIngestQueueData['query']>

type AiScopeParams = NonNullable<ListAiAccountsData['query']>

export type ListModelsParams = NonNullable<ListAiModelsData['query']>
type ListPricesParams = NonNullable<ListAiPricesData['query']>
export type { WebIngestPattern, WebIngestUrlFilter } from './generated'
export type {
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  IngestQueueMoveDirection,
  IngestQueueBulkAction,
  IngestQueueResponse,
}

type RecognitionPolicy = UpdateLibraryRecognitionPolicyRequest
type WebIngestPolicy = UpdateLibraryWebIngestPolicyRequest
type UpdateLibraryMcpSettingsRequest = {
  includeDocumentHintInMcpAnswers: boolean
}

function toGeneratedRequest<T extends object>(value: object): T {
  const body: Record<string, unknown> = {}
  for (const [key, fieldValue] of Object.entries(value)) {
    if (fieldValue !== undefined) {
      body[key] = fieldValue
    }
  }
  return body as T
}

const MODEL_AVAILABILITY_STATE: Record<ModelAvailabilityState, true> = {
  available: true,
  unavailable: true,
  unknown: true,
}

const PROVIDER_CAPABILITY_STATES = new Set(['supported', 'unsupported', 'unknown'])

function isModelAvailabilityState(value: unknown): value is ModelAvailabilityState {
  return typeof value === 'string' && value in MODEL_AVAILABILITY_STATE
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function assertStringEnumField(
  value: Record<string, unknown>,
  fieldName: string,
  policyName: string,
  allowedValues: Set<string>,
) {
  if (typeof value[fieldName] !== 'string' || !allowedValues.has(value[fieldName])) {
    throw new Error(`Provider catalog entry ${policyName}.${fieldName} is not canonical`)
  }
}

function assertRecordField(
  value: Record<string, unknown>,
  fieldName: string,
  providerId: string,
): Record<string, unknown> {
  const field = value[fieldName]
  if (!isRecord(field)) {
    throw new Error(`Provider catalog entry ${providerId}.${fieldName} must be an object`)
  }
  return field
}

export function parseModelCatalogResponse(payload: unknown): ModelCatalogEntryResponse[] {
  if (!Array.isArray(payload)) {
    throw new TypeError('Model catalog response must be an array')
  }

  for (const entry of payload) {
    if (!entry || typeof entry !== 'object') {
      throw new TypeError('Model catalog entry must be an object')
    }

    const model = entry as Partial<ModelCatalogEntryResponse>
    const id = typeof model.id === 'string' ? model.id : '<unknown>'
    if (!isModelAvailabilityState(model.availabilityState)) {
      throw new Error(`Model catalog entry ${id} has invalid availabilityState`)
    }
    if (!Array.isArray(model.availableAccountIds)) {
      throw new TypeError(`Model catalog entry ${id} has invalid availableAccountIds`)
    }
  }

  return payload as ModelCatalogEntryResponse[]
}

export function parseProviderCatalogResponse(payload: unknown): ProviderCatalogEntryResponse[] {
  if (!Array.isArray(payload)) {
    throw new TypeError('Provider catalog response must be an array')
  }

  for (const entry of payload) {
    if (!isRecord(entry)) {
      throw new TypeError('Provider catalog entry must be an object')
    }

    const id = typeof entry.id === 'string' ? entry.id : '<unknown>'
    resolveProviderCredentialPolicy({
      credentialPolicy: entry.credentialPolicy,
    })
    resolveProviderBaseUrlPolicy({ baseUrlPolicy: entry.baseUrlPolicy })
    resolveProviderModelDiscovery({ modelDiscovery: entry.modelDiscovery })

    const capabilities = assertRecordField(entry, 'capabilities', id)
    for (const capabilityName of [
      'chat',
      'embeddings',
      'modelDiscovery',
      'streaming',
      'tools',
      'vision',
    ]) {
      assertStringEnumField(
        capabilities,
        capabilityName,
        'capabilities',
        PROVIDER_CAPABILITY_STATES,
      )
    }
  }

  return payload as ProviderCatalogEntryResponse[]
}

export const ADMIN_MODEL_CATALOG_QUERY_KEY = ['admin', 'ai', 'models'] as const

export function adminModelCatalogQueryKey(params: ListModelsParams = {}) {
  return [...ADMIN_MODEL_CATALOG_QUERY_KEY, params] as const
}

export function adminModelCatalogOptions(params: ListModelsParams = {}) {
  return {
    queryKey: adminModelCatalogQueryKey(params),
    queryFn: () => adminApi.listModels(params),
  }
}

export type MintTokenRequest = GeneratedMintTokenRequest
export type CreateUserRequest = GeneratedCreateUserRequest
export type SetUserAccessRequest = GeneratedSetUserAccessRequest
export type { SystemRole, UserAccessResponse, UserResponse }

export const adminApi = {
  listTokens: () => Iam.listIamTokens({}).then((result) => unwrap<TokenResponse[]>(result)),
  listUsers: () => Iam.listIamUsers({}).then((result) => unwrap<UserResponse[]>(result)),
  createUser: (request: CreateUserRequest) =>
    Iam.createIamUser({ body: request }).then((result) => unwrap<UserResponse>(result)),
  setUserRole: (principalId: string, role: SystemRole) =>
    Iam.setIamUserRole({
      path: { principalId },
      body: { role },
    }).then((result) => unwrap<UserResponse>(result)),
  getUserAccess: (principalId: string) =>
    Iam.getIamUserAccess({ path: { principalId } }).then((result) =>
      unwrap<UserAccessResponse>(result),
    ),
  setUserAccess: (principalId: string, request: SetUserAccessRequest) =>
    Iam.setIamUserAccess({ path: { principalId }, body: request }).then((result) =>
      unwrap<UserAccessResponse>(result),
    ),
  mintToken: (request: MintTokenRequest) =>
    Iam.mintIamToken({ body: request }).then((result) => unwrap<MintTokenResponse>(result)),
  revokeToken: (principalId: string) =>
    Iam.revokeIamToken({ path: { tokenPrincipalId: principalId } }).then((result) => {
      unwrap(result)
    }),
  deleteToken: (principalId: string) =>
    Iam.deleteIamToken({ path: { tokenPrincipalId: principalId } }).then((result) => {
      unwrap(result)
    }),

  listProviders: () =>
    Ai.listAiProviders().then((result) => parseProviderCatalogResponse(unwrap(result))),
  createProvider: (data: CreateProviderRequest) =>
    Ai.createAiProvider({
      body: toGeneratedRequest<GeneratedCreateProviderRequest>(data),
    }).then((result) => unwrap<ProviderCatalogEntryResponse>(result)),
  updateProvider: (providerId: string, data: UpdateProviderRequest) =>
    Ai.updateAiProvider({
      path: { providerId },
      body: toGeneratedRequest<GeneratedUpdateProviderRequest>(data),
    }).then((result) => unwrap<ProviderCatalogEntryResponse>(result)),
  deleteProvider: (providerId: string) =>
    Ai.deleteAiProvider({ path: { providerId } }).then((result) => {
      unwrap(result)
    }),
  listModels: (params: ListModelsParams = {}) =>
    Ai.listAiModels({ query: params }).then((result) => parseModelCatalogResponse(unwrap(result))),
  createModel: (data: CreateModelRequest) =>
    Ai.createAiModel({
      body: toGeneratedRequest<GeneratedCreateModelRequest>(data),
    }).then((result) => unwrap<ModelCatalogEntryResponse>(result)),
  updateModel: (modelId: string, data: UpdateModelRequest) =>
    Ai.updateAiModel({
      path: { modelId },
      body: toGeneratedRequest<GeneratedUpdateModelRequest>(data),
    }).then((result) => unwrap<ModelCatalogEntryResponse>(result)),
  deleteModel: (modelId: string) =>
    Ai.deleteAiModel({ path: { modelId } }).then((result) => {
      unwrap(result)
    }),
  listAccounts: (params: AiScopeParams = {}) =>
    Ai.listAiAccounts({ query: params }).then((result) => unwrap<AiAccountResponse[]>(result)),
  createAccount: (data: CreateAccountRequest) =>
    Ai.createAiAccount({
      body: toGeneratedRequest<GeneratedCreateAccountRequest>(data),
    }).then((result) => unwrap<AiAccountResponse>(result)),
  updateAccount: (accountId: string, data: UpdateAccountRequest) =>
    Ai.updateAiAccount({
      path: { accountId },
      body: toGeneratedRequest<GeneratedUpdateAccountRequest>(data),
    }).then((result) => unwrap<AiAccountResponse>(result)),
  deleteAccount: (accountId: string) =>
    Ai.deleteAiAccount({ path: { accountId } }).then((result) => {
      unwrap(result)
    }),
  listBindings: (params: Required<Pick<AiScopeParams, 'scopeKind'>> & AiScopeParams) =>
    Ai.listAiLibraryBindings({ query: params }).then((result) =>
      unwrap<AiBindingResponse[]>(result),
    ),
  createBinding: (data: CreateBindingRequest) =>
    Ai.createAiLibraryBinding({
      body: toGeneratedRequest<GeneratedCreateBindingRequest>(data),
    }).then((result) => unwrap<AiBindingResponse>(result)),
  updateBinding: (bindingId: string, data: UpdateBindingRequest) =>
    Ai.updateAiLibraryBinding({
      path: { bindingId },
      body: toGeneratedRequest<GeneratedUpdateBindingRequest>(data),
    }).then((result) => unwrap<AiBindingResponse>(result)),
  deleteBinding: (bindingId: string) =>
    Ai.deleteAiLibraryBinding({ path: { bindingId } }).then((result) => {
      unwrap(result)
    }),
  validateBinding: (bindingId: string) =>
    Ai.validateAiLibraryBinding({ path: { bindingId } }).then((result) =>
      unwrap<BindingValidationResponse>(result),
    ),
  listPrices: (params: ListPricesParams = {}) =>
    Ai.listAiPrices({ query: params }).then((result) =>
      unwrap<PriceCatalogEntryResponse[]>(result),
    ),
  createPriceOverride: (data: CreatePriceOverrideRequest) =>
    Ai.createAiPriceOverride({
      body: toGeneratedRequest<GeneratedCreatePriceOverrideRequest>(data),
    }).then((result) => unwrap<PriceCatalogEntryResponse>(result)),
  updatePriceOverride: (priceId: string, data: UpdatePriceOverrideRequest) =>
    Ai.updateAiPriceOverride({
      path: { priceId },
      body: toGeneratedRequest<GeneratedUpdatePriceOverrideRequest>(data),
    }).then((result) => unwrap<PriceCatalogEntryResponse>(result)),
  deletePriceOverride: (priceId: string) =>
    Ai.deleteAiPriceOverride({ path: { priceId } }).then((result) => {
      unwrap(result)
    }),

  getAdminSurface: () =>
    Admin.getAdminSurface().then((result) => unwrap<AdminSurfaceResponse>(result)),

  listAuditEvents: (params: ListAuditEventsParams = {}) =>
    Audit.listAuditEvents({ query: params }).then((result) =>
      unwrap<AuditEventPageResponse>(result),
    ),
  listIngestQueue: (params: ListIngestQueueParams = {}) =>
    Ops.listIngestQueue({ query: params }).then((result) => unwrap<IngestQueueResponse>(result)),
  moveIngestQueueJob: (jobId: string, direction: IngestQueueMoveDirection) =>
    Ops.moveIngestQueueJob({ path: { jobId }, body: { direction } }).then((result) =>
      unwrap<IngestQueueResponse>(result),
    ),
  retryIngestQueueJob: (jobId: string) =>
    Ops.retryIngestQueueJob({ path: { jobId } }).then((result) =>
      unwrap<IngestQueueResponse>(result),
    ),
  bulkIngestQueueAction: (action: IngestQueueBulkAction, jobIds: string[]) =>
    Ops.bulkIngestQueueAction({ body: { action, jobIds } }).then((result) =>
      unwrap<BulkIngestQueueActionResponse>(result),
    ),
  pauseIngestQueueJob: (jobId: string) =>
    Ops.pauseIngestQueueJob({ path: { jobId } }).then((result) =>
      unwrap<IngestQueueResponse>(result),
    ),
  resumeIngestQueueJob: (jobId: string) =>
    Ops.resumeIngestQueueJob({ path: { jobId } }).then((result) =>
      unwrap<IngestQueueResponse>(result),
    ),
  cancelIngestQueueJob: (jobId: string) =>
    Ops.cancelIngestQueueJob({ path: { jobId } }).then((result) =>
      unwrap<IngestQueueResponse>(result),
    ),

  listWorkspaces: () =>
    Catalog.listCatalogWorkspaces().then((result) => unwrap<CatalogWorkspaceResponse[]>(result)),
  listLibraries: (workspaceId: string) =>
    Catalog.listCatalogLibraries({ path: { workspaceId } }).then((result) =>
      unwrap<CatalogLibraryResponse[]>(result),
    ),
  getLibrary: (libraryId: string) =>
    Catalog.getCatalogLibrary({ path: { libraryId } }).then((result) =>
      unwrap<CatalogLibraryResponse>(result),
    ),
  updateWebIngestPolicy: (libraryId: string, policy: WebIngestPolicy) =>
    Catalog.updateCatalogLibraryWebIngestPolicy({
      path: { libraryId },
      body: policy,
    }).then((result) => unwrap<CatalogLibraryResponse>(result)),
  updateRecognitionPolicy: (libraryId: string, policy: RecognitionPolicy) =>
    Catalog.updateCatalogLibraryRecognitionPolicy({
      path: { libraryId },
      body: policy,
    }).then((result) => unwrap<CatalogLibraryResponse>(result)),
  updateLibraryMcpSettings: async (libraryId: string, body: UpdateLibraryMcpSettingsRequest) => {
    const existing = unwrap<CatalogLibraryResponse>(
      await Catalog.getCatalogLibrary({ path: { libraryId } }),
    )
    return Catalog.updateCatalogLibrary({
      path: { libraryId },
      body: {
        slug: existing.slug,
        displayName: existing.displayName,
        description: existing.description ?? null,
        extractionPrompt: existing.extractionPrompt ?? null,
        lifecycleState: existing.lifecycleState,
        includeDocumentHintInMcpAnswers: body.includeDocumentHintInMcpAnswers,
      },
    }).then((result) => unwrap<CatalogLibraryResponse>(result))
  },
  createWorkspace: (name: string) =>
    Catalog.createCatalogWorkspace({ body: { displayName: name } }).then((result) =>
      unwrap<CatalogWorkspaceResponse>(result),
    ),
  createLibrary: (workspaceId: string, name: string) =>
    Catalog.createCatalogLibrary({
      path: { workspaceId },
      body: { displayName: name },
    }).then((result) => unwrap<CatalogLibraryResponse>(result)),
}
