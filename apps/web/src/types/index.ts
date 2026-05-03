// Core domain types for IronRAG

export interface User {
  id: string;
  login: string;
  displayName: string;
  accessLabel: string;
  role: "admin" | "operator" | "viewer";
}

export interface Workspace {
  id: string;
  name: string;
  createdAt: string;
}

export interface Library {
  id: string;
  workspaceId: string;
  name: string;
  createdAt: string;
  ingestionReady: boolean;
  queryReady: boolean;
  missingBindingPurposes: AIPurpose[];
}

export type AIPurpose =
  | "extract_graph"
  | "embed_chunk"
  | "query_compile"
  | "query_answer"
  | "vision";

/**
 * Canonical document status emitted by the backend list/detail endpoints.
 *
 * Values are derived server-side in `content_document_head` and map onto a
 * coarse user-visible state machine:
 *
 * - `in_progress` — worker is (or is about to be) moving the document forward.
 * - `attention`   — job is held but not progressing and needs an operator.
 * - `ready`       — document is usable for retrieval to some degree.
 * - `failed`      — terminal failure nothing will retry from automatically.
 * - `canceled`    — operator cancelled the job before it could produce output.
 */
/**
 * Canonical document status enum — mirrors the backend `derived_status`
 * CASE expression in `list_document_page_rows`. These are the only
 * values the list API will ever emit; the UI stays 1:1 with the server
 * so nothing has to re-derive buckets client-side.
 */
export type DocumentStatus =
  | "ready"
  | "processing"
  | "queued"
  | "failed"
  | "canceled";
export type DocumentReadiness =
  | "processing"
  | "readable"
  | "graph_sparse"
  | "graph_ready"
  | "failed";
export type SourceAccessKind = "stored_document" | "external_url";

export interface SourceAccess {
  kind: SourceAccessKind;
  href: string;
}

export interface DocumentItem {
  id: string;
  fileName: string;
  fileType: string;
  fileSize: number;
  uploadedAt: string;
  cost: number | null;
  status: DocumentStatus;
  readiness: DocumentReadiness;
  stage?: string;
  progressPercent?: number;
  processingStartedAt?: string;
  processingFinishedAt?: string;
  failureMessage?: string;
  statusReason?: string;
  canRetry?: boolean;
  sourceKind?:
    | "upload"
    | "web_page"
    | "append"
    | "edit"
    | "replace"
    | "connector_sync"
    | "import"
    | string;
  sourceUri?: string;
  sourceAccess?: SourceAccess;
}

export interface DocumentRevision {
  revisionNumber: number;
  mimeType: string;
  byteSize: number;
  title?: string;
  language?: string;
  sourceUri?: string;
  checksum?: string;
  storageKey?: string;
}

export interface DocumentDetail extends DocumentItem {
  canonicalId: string;
  activeRevision?: DocumentRevision;
  readableRevision?: number;
  latestMutation?: MutationRecord;
  latestSuccessfulAttempt?: string;
  webProvenance?: WebProvenance;
  mutations: MutationRecord[];
  attempts: ProcessingAttempt[];
  stageEvents: StageEvent[];
  preparationSummary?: PreparationSummary;
  preparedSegments: PreparedSegment[];
  technicalFacts: TechnicalFact[];
}

export interface MutationRecord {
  id: string;
  kind: string;
  state: "accepted" | "reconciling" | "completed" | "failed";
  createdAt: string;
  completedAt?: string;
  message?: string;
}

export interface ProcessingAttempt {
  id: string;
  stage: string;
  state: string;
  startedAt: string;
  completedAt?: string;
  error?: string;
}

export interface StageEvent {
  stage: string;
  state: string;
  timestamp: string;
  message?: string;
}

export interface PreparationSummary {
  readinessKind: DocumentReadiness;
  graphCoverageKind: string;
  typedFactCoverage?: string;
  lastProcessingStage?: string;
  preparationState: string;
  preparedSegmentCount: number;
  technicalFactCount: number;
  sourceFormat?: string;
  normalizationProfile?: string;
  updatedAt: string;
}

export interface PreparedSegment {
  ordinal: number;
  blockKind: string;
  headingTrail?: string;
  excerpt: string;
  pageNumber?: number;
  startOffset?: number;
  endOffset?: number;
  chunkCount: number;
  codeLanguage?: string;
  tableCoords?: string;
}

export interface TechnicalFact {
  id: string;
  factKind: string;
  canonicalValue: string;
  displayValue: string;
  qualifiers?: string[];
  supportSegments: number[];
  chunkCount: number;
  confidence: number;
  extractionKinds: string[];
  hasConflict: boolean;
  occurrenceCount: number;
  lastSeenAt: string;
}

export interface WebProvenance {
  runId: string;
  sourceUrl: string;
  discoveredAt: string;
}

// Web ingest types
export type WebIngestRunState =
  | "accepted"
  | "discovering"
  | "processing"
  | "completed"
  | "completed_partial"
  | "failed"
  | "canceled";
export type WebIngestPageState =
  | "discovered"
  | "eligible"
  | "duplicate"
  | "excluded"
  | "blocked"
  | "queued"
  | "processing"
  | "processed"
  | "failed"
  | "canceled";

export interface WebIngestRun {
  id: string;
  seedUrl: string;
  mode: "single_page" | "recursive_crawl";
  boundaryPolicy: "same_host" | "allow_external";
  maxDepth: number;
  maxPages: number;
  state: WebIngestRunState;
  totalPages: number;
  processedPages: number;
  failedPages: number;
  createdAt: string;
  updatedAt: string;
}

export interface WebIngestPage {
  id: string;
  url: string;
  state: WebIngestPageState;
  depth: number;
  discoveredAt: string;
  processedAt?: string;
  error?: string;
}

// Graph types
export type GraphStatus =
  | "empty"
  | "building"
  | "rebuilding"
  | "ready"
  | "partial"
  | "failed"
  | "stale";
export type GraphNodeType =
  | "document"
  | "person"
  | "organization"
  | "location"
  | "event"
  | "artifact"
  | "natural"
  | "process"
  | "concept"
  | "attribute"
  | "entity";

export interface GraphNode {
  id: string;
  label: string;
  type: GraphNodeType;
  subType?: string;
  summary?: string;
  canonicalSummary?: string;
  properties: Record<string, string>;
  edgeCount: number;
  convergenceStatus?: string;
  warnings?: string[];
  sourceDocumentIds?: string[];
}

export interface GraphEdge {
  id: string;
  sourceId: string;
  targetId: string;
  label: string;
  weight: number;
}

export interface GraphMetadata {
  nodeCount: number;
  edgeCount: number;
  hiddenDisconnectedCount: number;
  status: GraphStatus;
  convergenceStatus: string;
  recommendedLayout?: string;
}

// Assistant types
export interface AssistantSession {
  id: string;
  libraryId: string;
  title: string;
  updatedAt: string;
  turnCount: number;
}

export type AssistantStage = "planning" | "grounding" | "response";

export interface AssistantMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  timestamp: string;
  executionId?: string | null;
  attachments?: FileAttachment[];
  stage?: AssistantStage;
  isStreaming?: boolean;
  evidence?: EvidenceBundle;
}

export interface FileAttachment {
  id: string;
  name: string;
  size: number;
  type: string;
}

export interface EvidenceBundle {
  segmentRefs: SegmentReference[];
  factRefs: FactReference[];
  entityRefs: EntityReference[];
  relationRefs: RelationReference[];
  verificationState: VerificationState;
  verificationWarnings: string[];
  runtimeSummary?: RuntimeSummary;
}

export interface SegmentReference {
  documentId: string;
  documentName: string;
  documentTitle: string | null;
  sourceUri: string | null;
  sourceAccess: SourceAccess | null;
  segmentOrdinal: number;
  excerpt: string;
  relevance: number;
}

export interface FactReference {
  factKind: string;
  value: string;
  confidence: number;
  documentName: string;
}

export interface EntityReference {
  entityId: string;
  label: string;
  type: string;
  relevance: number;
}

export interface RelationReference {
  sourceLabel: string;
  targetLabel: string;
  relation: string;
  weight: number;
}

export type VerificationState =
  | "passed"
  | "partially_supported"
  | "conflicting"
  | "insufficient_evidence"
  | "failed"
  | "not_run";

export interface RuntimeSummary {
  totalSegments: number;
  totalFacts: number;
  totalEntities: number;
  totalRelations: number;
  stages: RuntimeStageSummary[];
  policyInterventions: PolicyIntervention[];
}

export interface RuntimeStageSummary {
  stage: string;
  durationMs: number;
  itemCount: number;
}

export interface PolicyIntervention {
  kind: "rejected" | "terminated" | "blocked";
  reason: string;
  timestamp: string;
}

// Admin types
export interface APIToken {
  id: string;
  label: string;
  tokenPrefix: string;
  status: "active" | "expired" | "revoked";
  expiresAt?: string;
  revokedAt?: string;
  issuedBy?: TokenIssuer;
  lastUsedAt?: string;
  scope: TokenScope;
  grants: TokenGrant[];
}

export interface TokenIssuer {
  id: string;
  displayLabel: string;
}

export interface TokenScopeWorkspace {
  id: string;
  displayName: string;
}

export interface TokenScopeLibrary {
  id: string;
  workspaceId: string;
  displayName: string;
}

export interface TokenScope {
  kind: "system" | "workspace" | "library";
  workspace?: TokenScopeWorkspace;
  libraries: TokenScopeLibrary[];
}

export interface TokenGrant {
  resourceKind: string;
  resourceId: string;
  permission: string;
  workspace?: TokenScopeWorkspace;
  library?: TokenScopeLibrary;
}

export type AIScopeKind = "instance" | "workspace" | "library";

export interface AIProvider {
  id: string;
  displayName: string;
  kind: string;
  apiStyle: string;
  lifecycleState: "active" | "deprecated" | "preview";
  defaultBaseUrl?: string;
  apiKeyRequired: boolean;
  baseUrlRequired: boolean;
  modelCount: number;
  credentialCount: number;
}

export interface AICredential {
  id: string;
  scopeKind: AIScopeKind;
  workspaceId?: string;
  libraryId?: string;
  providerId: string;
  providerName: string;
  providerKind: string;
  label: string;
  state: "active" | "invalid" | "revoked" | "unchecked";
  createdAt: string;
  updatedAt: string;
  baseUrl?: string;
  apiKeySummary: string;
}

export type AIModelAvailabilityState = "available" | "unavailable" | "unknown";

export interface AIModelOption {
  id: string;
  providerCatalogId: string;
  modelName: string;
  capabilityKind: string;
  modalityKind: string;
  allowedBindingPurposes: AIPurpose[];
  contextWindow?: number;
  maxOutputTokens?: number;
  availabilityState: AIModelAvailabilityState;
  availableCredentialIds: string[];
}

export interface ModelPreset {
  id: string;
  scopeKind: AIScopeKind;
  workspaceId?: string;
  libraryId?: string;
  providerId: string;
  providerName: string;
  providerKind: string;
  modelCatalogId: string;
  modelName: string;
  presetName: string;
  allowedBindingPurposes: AIPurpose[];
  systemPrompt?: string;
  temperature?: number;
  topP?: number;
  maxOutputTokens?: number;
  extraParams?: Record<string, unknown>;
  createdAt: string;
  updatedAt: string;
}

export interface AIBindingAssignment {
  id: string;
  scopeKind: AIScopeKind;
  workspaceId?: string;
  libraryId?: string;
  purpose: AIPurpose;
  credentialId: string;
  presetId: string;
  state: "configured" | "inactive" | "invalid";
}

export interface BindingValidation {
  state: "valid" | "invalid" | "unchecked";
  checkedAt?: string;
  failureCode?: string;
  message?: string;
}

export interface PricingRule {
  id: string;
  provider: string;
  model: string;
  billingUnit: string;
  unitPrice: number;
  currency: string;
  effectiveFrom: string;
  effectiveTo?: string;
  priceVariant?: string;
  inputTokenMin?: number;
  inputTokenMax?: number;
  sourceOrigin: string;
}

export interface OperationsSnapshot {
  queueDepth: number;
  runningAttempts: number;
  readableDocCount: number;
  failedDocCount: number;
  status: "healthy" | "processing" | "rebuilding" | "degraded";
  knowledgeGenerationState: string;
  lastRecomputedAt: string;
  warnings: OperationsWarning[];
}

export interface OperationsWarning {
  id: string;
  warningKind: string;
  severity: string;
  createdAt: string;
  resolvedAt?: string;
}

export interface AuditAssistantModel {
  providerKind: string;
  modelName: string;
}

export interface AuditAssistantCall {
  queryExecutionId: string;
  conversationId?: string;
  runtimeExecutionId?: string;
  models: AuditAssistantModel[];
  totalCost?: string | number | null;
  currencyCode?: string | null;
  providerCallCount: number;
}

export interface AuditEvent {
  id: string;
  action: string;
  resultKind: "succeeded" | "rejected" | "failed";
  surfaceKind: string;
  timestamp: string;
  message: string;
  subjectSummary: string;
  actor: string;
  assistantCall?: AuditAssistantCall;
}

export interface AuditEventPage {
  items: AuditEvent[];
  total: number;
  limit: number;
  offset: number;
}

export interface DocumentLifecycle {
  totalCost?: number | string | null;
  currencyCode?: string | null;
  attempts: DocumentAttempt[];
}

export interface DocumentAttempt {
  jobId: string;
  attemptNo: number;
  attemptKind: string;
  status: string;
  queueStartedAt: string;
  startedAt?: string | null;
  finishedAt?: string | null;
  totalElapsedMs?: number | null;
  stageEvents: DocumentStageEvent[];
}

export interface DocumentStageEvent {
  stage: string;
  status: string;
  startedAt: string;
  finishedAt?: string | null;
  elapsedMs?: number | null;
  providerKind?: string | null;
  modelName?: string | null;
  promptTokens?: number | null;
  completionTokens?: number | null;
  totalTokens?: number | null;
  estimatedCost?: number | null;
  currencyCode?: string | null;
}

export type Locale = string;

export interface LocaleOption {
  code: string;
  label: string;
  nativeLabel: string;
}

export const AVAILABLE_LOCALES: LocaleOption[] = [
  { code: "en", label: "English", nativeLabel: "English" },
  { code: "ru", label: "Russian", nativeLabel: "Русский" },
];
