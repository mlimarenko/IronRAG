/** Raw provider catalog entry from GET /v1/ai/providers */
export interface RawProviderCatalogEntry {
  id: string;
  providerKind: string;
  displayName: string;
  apiStyle?: string;
  credentialSource: string;
  defaultBaseUrl?: string;
  apiKeyRequired: boolean;
  baseUrlRequired: boolean;
  lifecycleState?: string;
}

/** Raw credential from GET /v1/ai/credentials */
export interface RawProviderCredentialResponse {
  id: string;
  scopeKind?: string;
  workspaceId?: string;
  libraryId?: string;
  providerCatalogId: string;
  label: string;
  baseUrl?: string;
  apiKeyConfigured?: boolean;
  apiKeySummary?: string;
  credentialState?: string;
  createdAt?: string;
  updatedAt?: string;
}

/** Raw model preset from GET /v1/ai/presets */
export interface RawModelPresetResponse {
  id: string;
  scopeKind?: string;
  workspaceId?: string;
  libraryId?: string;
  modelCatalogId: string;
  bindingPurpose: string;
  presetName: string;
  systemPrompt?: string;
  temperature?: number;
  topP?: number;
  maxOutputTokensOverride?: number;
  extraParametersJson?: string;
  createdAt?: string;
  updatedAt?: string;
}

/** Raw binding assignment from GET /v1/ai/bindings */
export interface RawBindingAssignmentResponse {
  id: string;
  scopeKind?: string;
  workspaceId?: string;
  libraryId?: string;
  bindingPurpose: string;
  providerCredentialId: string;
  modelPresetId: string;
  bindingState?: string;
}

/** Raw model catalog entry from GET /v1/ai/models */
export interface RawModelCatalogEntry {
  id: string;
  providerCatalogId: string;
  modelName: string;
  displayName?: string;
  defaultRoles?: string;
  allowedBindingPurposes?: string[];
  capabilityKind?: string;
  modalityKind?: string;
  contextWindow?: number;
  maxOutputTokens?: number;
  availabilityState?: 'available' | 'unavailable' | 'unknown';
  availableCredentialIds?: string[];
}

/** Raw API token response */
export interface RawTokenResponse {
  id?: string;
  principalId?: string;
  workspaceId?: string;
  label?: string;
  tokenPrefix?: string;
  status?: string;
  expiresAt?: string;
  revokedAt?: string;
  issuedByPrincipalId?: string;
  lastUsedAt?: string;
}

/** Raw pricing rule response */
export interface RawPricingResponse {
  id: string;
  modelCatalogId?: string;
  billingUnit?: string;
  unitPrice?: string;
  currencyCode?: string;
  effectiveFrom?: string;
  effectiveTo?: string;
  catalogScope?: string;
}

/** Raw operations warning */
export interface RawOperationsWarning {
  id?: string;
  warningKind?: string;
  severity?: string;
  createdAt?: string;
  resolvedAt?: string;
}

/** Raw ops state block */
export interface RawOperationsState {
  queueDepth?: number;
  runningAttempts?: number;
  readableDocumentCount?: number;
  failedDocumentCount?: number;
  degradedState?: string;
  knowledgeGenerationState?: string;
  lastRecomputedAt?: string;
}

/** Raw operations snapshot response */
export interface RawOpsResponse {
  state?: RawOperationsState;
  warnings?: RawOperationsWarning[];
}

/** Raw audit subject */
export interface RawAuditSubject {
  subjectKind: string;
  subjectId: string;
}

/** Raw audit event response */
export interface RawAuditEventResponse {
  id: string;
  actionKind?: string;
  resultKind?: string;
  surfaceKind?: string;
  createdAt?: string;
  redactedMessage?: string;
  subjects?: RawAuditSubject[];
  actorPrincipalId?: string;
}

/** Raw audit events page response */
export interface RawAuditPageResponse {
  items?: RawAuditEventResponse[];
  total?: number;
  limit?: number;
  offset?: number;
}

/** Raw prepared segment reference from assistant turn response */
export interface RawPreparedSegmentReference {
  documentId?: string;
  segmentId?: string;
  documentTitle?: string | null;
  sourceUri?: string | null;
  sourceAccess?: unknown;
  headingTrail?: unknown[];
  sectionPath?: unknown[];
  blockKind?: string;
  rank?: number;
  score?: number;
}

/** Raw technical fact reference */
export interface RawTechnicalFactReference {
  factKind: string;
  displayValue?: unknown;
  canonicalValue?: unknown;
  score?: number;
}

/** Raw entity reference */
export interface RawEntityReference {
  nodeId: string;
  label?: unknown;
  entityType?: string;
  score?: number;
}

/** Raw relation reference */
export interface RawRelationReference {
  predicate?: string;
  normalizedAssertion?: string;
  score?: number;
}

/** Raw verification warning */
export interface RawVerificationWarning {
  message?: string;
  code?: string;
}

/** Raw runtime stage summary */
export interface RawRuntimeStageSummary {
  stageKind: string;
}

/** Raw assistant turn response */
export interface RawAssistantTurnResponse {
  preparedSegmentReferences?: RawPreparedSegmentReference[];
  technicalFactReferences?: RawTechnicalFactReference[];
  entityReferences?: RawEntityReference[];
  relationReferences?: RawRelationReference[];
  verificationState: string;
  verificationWarnings?: RawVerificationWarning[];
  runtimeStageSummaries?: RawRuntimeStageSummary[];
}

/** Raw assistant session */
export interface RawAssistantSession {
  id: string;
  libraryId: string;
  title?: string;
  updatedAt: string;
  turnCount?: number;
}

/** Raw assistant message */
export interface RawAssistantMessage {
  id: string;
  role: string;
  content?: string;
  timestamp: string;
}

/** Raw API token mint response (POST /v1/iam/tokens) */
export interface RawTokenMintResponse {
  token: string;
  api_token?: RawTokenResponse;
}

/** Raw knowledge entity list item */
export interface RawKnowledgeEntity {
  id?: string;
  entityId?: string;
  key?: string;
  label?: string;
  canonicalLabel?: string;
  entityType?: string;
  entitySubType?: string;
  summary?: string | null;
  supportCount?: number;
  confidence?: number;
  entityState?: string;
  aliases?: string[];
  nodeType?: string;
}

/** Raw knowledge relation list item */
export interface RawKnowledgeRelation {
  id?: string;
  relationId?: string;
  subjectEntityId: string;
  objectEntityId: string;
  predicate?: string;
  supportCount?: number;
}

/** Raw knowledge document list item (from /knowledge/libraries/{id}/documents) */
export interface RawKnowledgeDocument {
  id?: string;
  document_id?: string;
  documentId?: string;
  title?: string;
  fileName?: string;
  external_key?: string;
  document_state?: string;
}

/** Raw topology document link */
export interface RawGraphDocumentLink {
  documentId: string;
  targetNodeId: string;
  supportCount?: number;
}

/** Raw supporting document reference on an entity detail */
export interface RawSupportingDocument {
  documentId: string;
}

/** Raw knowledge entity detail shape returned by GET /knowledge/libraries/{id}/entities/{entityId} */
export interface RawKnowledgeEntityDetail {
  entity?: RawKnowledgeEntity;
  selectedNode?: {
    relatedNodes?: unknown[];
    supportingDocuments?: RawSupportingDocument[];
  };
  // The endpoint sometimes returns the entity fields inline at the top level as well
  id?: string;
  entityId?: string;
  label?: string;
  canonicalLabel?: string;
  entityType?: string;
  nodeType?: string;
  summary?: string;
  supportCount?: number;
  confidence?: number;
  entityState?: string;
  aliases?: string[];
  properties?: Iterable<[string, unknown]>;
  relatedNodes?: unknown[];
  supportingDocuments?: RawSupportingDocument[];
}

/** Raw web ingest run list item */
export interface RawWebIngestRunListItem {
  runId: string;
  seedUrl: string;
  runState: string;
  mode: string;
  boundaryPolicy?: string;
  maxDepth?: number;
  maxPages?: number;
  counts?: {
    discovered?: number;
    processed?: number;
  };
}

/** Raw web ingest run page item */
export interface RawWebIngestRunPage {
  normalizedUrl?: string;
  discoveredUrl?: string;
  candidateState?: string;
}

/** Envelope for list endpoints that may return `{ items: [...] }` */
export interface RawListEnvelope<T> {
  items?: T[];
}
