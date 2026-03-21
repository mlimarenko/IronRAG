export type AdminTab = 'tokens' | 'aiCatalog' | 'pricing' | 'audit'

export type AdminGrantResourceKind =
  | 'system'
  | 'workspace'
  | 'library'
  | 'document'
  | 'connector'
  | 'provider_credential'
  | 'library_binding'

export type AdminPermissionKind =
  | 'workspace_admin'
  | 'workspace_read'
  | 'library_read'
  | 'library_write'
  | 'document_read'
  | 'document_write'
  | 'connector_admin'
  | 'credential_admin'
  | 'binding_admin'
  | 'query_run'
  | 'ops_read'
  | 'audit_read'
  | 'iam_admin'

export interface AdminTabCounts {
  tokens: number
  aiCatalog: number
  pricing: number
  audit: number
}

export interface AdminTabAvailability {
  tokens: boolean
  aiCatalog: boolean
  pricing: boolean
  audit: boolean
}

export interface AdminWorkspaceMembership {
  workspaceId: string
  principalId: string
  membershipState: string
  joinedAt: string
  endedAt: string | null
}

export interface AdminGrant {
  id: string
  principalId: string
  resourceKind: AdminGrantResourceKind
  resourceId: string
  permissionKind: AdminPermissionKind
  grantedByPrincipalId: string | null
  grantedAt: string
  expiresAt: string | null
}

export interface AdminPrincipalSummary {
  id: string
  principalKind: string
  status: string
  displayLabel: string
  email: string | null
  displayName: string | null
  authProviderKind: string | null
  externalSubject: string | null
  workspaceMemberships: AdminWorkspaceMembership[]
  effectiveGrants: AdminGrant[]
}

export interface AdminApiTokenRow {
  principalId: string
  workspaceId: string | null
  label: string
  tokenPrefix: string
  status: string
  expiresAt: string | null
  revokedAt: string | null
  issuedByPrincipalId: string | null
  lastUsedAt: string | null
  plaintextToken: string | null
  grants: AdminGrant[]
}

export interface CreateApiTokenPayload {
  workspaceId: string
  label: string
  expiresInDays: number | null
  grantResourceKind: Extract<AdminGrantResourceKind, 'workspace' | 'library'>
  grantResourceId: string
  permissionKinds: AdminPermissionKind[]
}

export interface CreateApiTokenResult {
  row: AdminApiTokenRow
  plaintextToken: string
}

export interface AdminProviderCatalogEntry {
  id: string
  providerKind: string
  displayName: string
  apiStyle: string
  lifecycleState: string
}

export interface AdminModelCatalogEntry {
  id: string
  providerCatalogId: string
  modelName: string
  capabilityKind: string
  modalityKind: string
  contextWindow: number | null
  maxOutputTokens: number | null
}

export interface AdminPriceCatalogEntry {
  id: string
  modelCatalogId: string
  billingUnit: string
  unitPrice: string
  currencyCode: string
  effectiveFrom: string
}

export interface AdminProviderCredential {
  id: string
  workspaceId: string
  providerCatalogId: string
  label: string
  secretRef: string
  credentialState: string
  createdAt: string
  updatedAt: string
}

export interface AdminModelPreset {
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

export interface AdminBindingValidation {
  id: string
  bindingId: string
  validationState: string
  checkedAt: string
  failureCode: string | null
  message: string | null
}

export interface AdminLibraryBinding {
  id: string
  workspaceId: string
  libraryId: string
  bindingPurpose: string
  providerCredentialId: string
  modelPresetId: string
  bindingState: string
  latestValidation: AdminBindingValidation | null
}

export interface CreateAdminCredentialPayload {
  workspaceId: string
  providerCatalogId: string
  label: string
  secretRef: string
}

export interface AdminAiConsoleState {
  workspaceId: string
  workspaceName: string
  libraryId: string
  libraryName: string
  providers: AdminProviderCatalogEntry[]
  models: AdminModelCatalogEntry[]
  modelPresets: AdminModelPreset[]
  prices: AdminPriceCatalogEntry[]
  credentials: AdminProviderCredential[]
  bindings: AdminLibraryBinding[]
}

export interface AdminOpsAsyncOperation {
  id: string
  workspaceId: string
  libraryId: string | null
  operationKind: string
  status: string
  surfaceKind: string | null
  subjectKind: string | null
  subjectId: string | null
  failureCode: string | null
  createdAt: string
  completedAt: string | null
}

export interface AdminKnowledgeGeneration {
  key: string
  bundleId?: string | null
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

export interface AdminOpsLibraryState {
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

export interface AdminOpsLibraryWarning {
  id: string
  libraryId: string
  warningKind: string
  severity: string
  createdAt: string
  resolvedAt: string | null
}

export interface AdminOpsLibrarySnapshot {
  state: AdminOpsLibraryState
  knowledgeGenerations: AdminKnowledgeGeneration[]
  warnings: AdminOpsLibraryWarning[]
}

export interface AdminAuditEventSubject {
  auditEventId: string
  subjectKind: string
  subjectId: string
  workspaceId: string | null
  libraryId: string | null
  documentId: string | null
}

export interface AdminAuditEvent {
  id: string
  actorPrincipalId: string | null
  surfaceKind: string
  actionKind: string
  resultKind: string
  requestId: string | null
  traceId: string | null
  createdAt: string
  redactedMessage: string | null
  internalMessage: string | null
  subjects: AdminAuditEventSubject[]
}
