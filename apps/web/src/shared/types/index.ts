import type {
  AiBindingPurpose,
  AssistantVerificationState,
  DocumentReadiness as GeneratedDocumentReadiness,
  DocumentStatus as GeneratedDocumentStatus,
  GraphStatus as GeneratedGraphStatus,
  ProviderCapabilities as GeneratedProviderCapabilities,
} from '@/shared/api/generated'
import type { DocumentFailureNotice } from '@/shared/lib/document-processing'

// Core domain types for IronRAG

export interface User {
  id: string
  login: string
  displayName: string
  accessLabel: string
  role: 'admin' | 'operator' | 'viewer'
}

export interface Workspace {
  id: string
  name: string
  createdAt: string
}

export interface Library {
  id: string
  workspaceId: string
  name: string
  createdAt: string
  includeDocumentHintInMcpAnswers: boolean
  ingestionReady: boolean
  queryReady: boolean
  missingBindingPurposes: AiBindingPurpose[]
}

/**
 * Document status enum — mirrors the backend `derived_status`
 * CASE expression in `list_document_page_rows`. These are the only
 * values the list API will ever emit; the UI stays 1:1 with the server
 * so nothing has to re-derive buckets client-side.
 */
type DocumentStatus = GeneratedDocumentStatus
type DocumentReadiness = GeneratedDocumentReadiness
type SourceAccessKind = 'stored_document' | 'external_url'

export interface SourceAccess {
  kind: SourceAccessKind
  href: string
}

export interface DocumentItem {
  id: string
  documentState: string
  fileName: string | null
  fileType: string | null
  fileSize: number | null
  uploadedAt: string
  completedAt?: string | undefined
  cost: string | null
  /// Set when the owning library rollup was reconciling at read time: a null
  /// cost then means "not known yet", and the grid names that state instead of
  /// rendering a bare dash that reads as "no cost".
  costRollupFreshness?: 'current' | 'reconciling' | undefined
  status: DocumentStatus
  readiness: DocumentReadiness
  stage?: string | undefined
  progressPercent?: number | undefined
  processingStartedAt?: string | undefined
  processingFinishedAt?: string | undefined
  failureCode?: string | undefined
  failureMessage?: string | undefined
  statusReason?: string | undefined
  failureNotice?: DocumentFailureNotice | undefined
  canRetry?: boolean | undefined
  documentHint?: string | undefined
  externalKey?: string | undefined
  sourceKind?: string | undefined
  sourceUri?: string | undefined
  sourceAccess?: SourceAccess | undefined
}

// Graph types
export type GraphStatus = GeneratedGraphStatus
export type GraphNodeType =
  | 'document'
  | 'person'
  | 'organization'
  | 'location'
  | 'event'
  | 'artifact'
  | 'natural'
  | 'process'
  | 'concept'
  | 'attribute'
  | 'entity'

export interface GraphNode {
  id: string
  label: string
  type: GraphNodeType
  subType?: string
  summary?: string
  documentSummary?: string
  properties: Record<string, string>
  edgeCount: number
  convergenceStatus?: string
  warnings?: string[]
  sourceDocumentIds?: string[]
}

export interface GraphEdge {
  id: string
  sourceId: string
  targetId: string
  label: string
  weight: number
}

export interface GraphMetadata {
  nodeCount: number
  edgeCount: number
  hiddenDisconnectedCount: number
  status: GraphStatus
  convergenceStatus: string
  recommendedLayout?: string
}

// Assistant types
export interface AssistantSession {
  id: string
  libraryId: string
  title: string
  updatedAt: string
  turnCount: number
}

type AssistantAgentActivityEventType =
  | 'started'
  | 'model_request'
  | 'model_response'
  | 'tool_call_started'
  | 'tool_call_finished'
  | 'working'
  | 'persisting'

export type AssistantAgentActivityEvent = {
  type: AssistantAgentActivityEventType
  deadline_ms?: number
  iteration?: number
  provider_kind?: string
  model_name?: string
  tool_call_count?: number
  has_final_answer?: boolean
  tool_name?: string
  is_error?: boolean
  child_execution_id?: string | null
  result_preview?: string
  elapsed_ms?: number
}

export interface AssistantMessage {
  id: string
  role: 'user' | 'assistant'
  content: string
  timestamp: string
  /** Latest processing time measured and reported by the server. */
  elapsedMs?: number
  /**
   * Server-authoritative runtime wall-clock in milliseconds
   * (`runtimeSummary.completedAt − runtimeSummary.acceptedAt`). Absent until the
   * runtime has a valid terminal timestamp pair.
   */
  durationMs?: number
  executionId?: string | null
  evidence?: EvidenceBundle
  activityEvents?: AssistantAgentActivityEvent[]
}

export interface EvidenceBundle {
  segmentRefs: SegmentReference[]
  factRefs: FactReference[]
  entityRefs: EntityReference[]
  relationRefs: RelationReference[]
  verificationState: VerificationState
  verificationWarnings: string[]
  answerDisposition: AssistantAnswerDisposition
  clarification: AssistantClarification
  runtimeSummary?: RuntimeSummary
}

type AssistantAnswerDisposition =
  'non_terminal' | 'factual_ready' | 'safe_fallback' | 'clarification'

interface AssistantClarification {
  required: boolean
  question: string | null
  answerCandidates: AssistantAnswerCandidate[]
}

interface AssistantAnswerCandidate {
  label: string
  kind: string
  confidence: number | null
  provenance: {
    entityId: string | null
    documentId: string | null
    chunkId: string | null
  }
}

interface SegmentReference {
  documentId: string
  documentName: string
  documentTitle: string | null
  sourceUri: string | null
  sourceAccess: SourceAccess | null
  segmentOrdinal: number
  excerpt: string
  relevance: number
}

interface FactReference {
  factKind: string
  value: string
  confidence: number
  documentName: string
}

interface EntityReference {
  entityId: string
  label: string
  type: string
  relevance: number
}

interface RelationReference {
  sourceLabel: string
  targetLabel: string
  relation: string
  weight: number
}

export type VerificationState = AssistantVerificationState

interface RuntimeSummary {
  totalSegments: number
  totalFacts: number
  totalEntities: number
  totalRelations: number
  stages: RuntimeStageSummary[]
}

interface RuntimeStageSummary {
  stage: string
  durationMs: number
}

// Admin types
export interface APIToken {
  id: string
  label: string
  tokenPrefix: string
  status: 'active' | 'expired' | 'revoked'
  expiresAt?: string
  revokedAt?: string
  issuedBy?: TokenIssuer
  lastUsedAt?: string
  scope: TokenScope
  grants: TokenGrant[]
}

interface TokenIssuer {
  id: string
  displayLabel: string
}

interface TokenScopeWorkspace {
  id: string
  displayName: string
}

interface TokenScopeLibrary {
  id: string
  workspaceId: string
  displayName: string
}

interface TokenScope {
  kind: 'system' | 'workspace' | 'library'
  workspace?: TokenScopeWorkspace
  libraries: TokenScopeLibrary[]
}

interface TokenGrant {
  resourceKind: string
  resourceId: string
  permission: string
  workspace?: TokenScopeWorkspace
  library?: TokenScopeLibrary
}

export type AIScopeKind = 'instance' | 'workspace' | 'library'

type AIProviderCapabilities = Partial<GeneratedProviderCapabilities>
type AIProviderRuntime = Record<string, unknown>
type AIProviderUiHints = Record<string, unknown>
type AIProviderBaseUrlMode = 'fixed' | 'required' | 'optional'
type AIProviderCredentialValidationMode = 'chat_round_trip' | 'model_list' | 'none'

export interface AIProviderCredentialPolicy {
  apiKeyRequired: boolean
  baseUrlRequired: boolean
  baseUrlMode: AIProviderBaseUrlMode
  validationMode: AIProviderCredentialValidationMode
}

export interface AIProviderBaseUrlPolicy {
  allowOverride: boolean
  requireHttps: boolean
  allowPrivateNetwork: boolean
  trimSuffixes: string[]
}

export interface AIProviderModelDiscovery {
  mode: 'shared' | 'credential' | 'unsupported'
  paths: Array<{
    capabilityKind: string
    path: string
  }>
}

export interface AIProvider {
  id: string
  displayName: string
  kind: string
  apiStyle: string
  lifecycleState: 'active' | 'deprecated' | 'preview' | 'disabled'
  defaultBaseUrl?: string
  apiKeyRequired: boolean
  baseUrlRequired: boolean
  credentialPolicy: AIProviderCredentialPolicy
  baseUrlPolicy: AIProviderBaseUrlPolicy
  modelDiscovery: AIProviderModelDiscovery
  capabilities: AIProviderCapabilities
  runtime: AIProviderRuntime
  uiHints: AIProviderUiHints
  /** Purposes no seeded model covers, each with why and which kind of gap it is. */
  purposeGaps: AIProviderPurposeGap[]
  modelCount: number
  credentialCount: number
}

export type { AiBindingPurpose }

/**
 * Either the provider is incapable, or it can serve the purpose and the model
 * is the operator's to choose. The two need opposite words on screen: one is a
 * dead end, the other is a prompt to pick from the provider's live listing.
 */
type AIProviderPurposeGapKind = 'provider_cannot' | 'operator_supplies'

interface AIProviderPurposeGap {
  purpose: string
  kind: AIProviderPurposeGapKind
  /** Stable code to translate; `reason` is the server's English fallback. */
  code: string
  reason: string
}

export interface AIAccount {
  id: string
  scopeKind: AIScopeKind
  workspaceId?: string
  libraryId?: string
  providerId: string
  providerName: string
  providerKind: string
  provider?: AIProvider
  label: string
  state: 'active' | 'invalid' | 'revoked' | 'unchecked'
  createdAt: string
  updatedAt: string
  baseUrl?: string
  apiKeySummary: string
}

type AIModelAvailabilityState = 'available' | 'unavailable' | 'unknown'

export interface AIModelOption {
  id: string
  providerCatalogId: string
  modelName: string
  allowedBindingPurposes: AiBindingPurpose[]
  lifecycleState?: 'active' | 'deprecated' | 'preview' | 'disabled'
  contextWindow?: number
  maxOutputTokens?: number
  availabilityState: AIModelAvailabilityState
  availableAccountIds: string[]
  /**
   * True for rows this build seeded, whose purpose list is a curated statement
   * of what the model can do. A row an operator added carries no such claim —
   * its purposes are simply the ones bound so far, and adding another is a
   * normal edit rather than a contradiction.
   */
  seeded: boolean
}

export type AIBindingState = 'active' | 'invalid' | 'disabled'

export interface AIBinding {
  id: string
  scopeKind: AIScopeKind
  workspaceId?: string
  libraryId?: string
  purpose: AiBindingPurpose
  accountId: string
  modelCatalogId: string
  systemPrompt?: string
  temperature?: number
  topP?: number
  maxOutputTokens?: number
  extraParams?: Record<string, unknown>
  state: AIBindingState
}

export interface OperationsSnapshot {
  queueDepth: number
  runningAttempts: number
  readableDocCount: number
  failedDocCount: number
  status: 'healthy' | 'processing' | 'rebuilding' | 'degraded'
  knowledgeGenerationState: string
  lastRecomputedAt: string
  warnings: OperationsWarning[]
}

export interface OperationsWarning {
  id: string
  warningKind: string
  severity: string
  createdAt: string
  resolvedAt?: string
}

interface AuditAssistantModel {
  providerKind: string
  modelName: string
}

interface AuditAssistantCall {
  queryExecutionId: string
  conversationId?: string
  runtimeExecutionId?: string
  models: AuditAssistantModel[]
  totalCost?: string | number | null
  currencyCode?: string | null
  providerCallCount: number
}

export interface AuditEvent {
  id: string
  action: string
  resultKind: 'succeeded' | 'rejected' | 'failed'
  surfaceKind: string
  timestamp: string
  message: string
  subjectSummary: string
  actor: string
  assistantCall?: AuditAssistantCall
}

export interface AuditEventPage {
  items: AuditEvent[]
  nextCursor: string | null
  total: number
}

interface LocaleOption {
  code: string
  label: string
  nativeLabel: string
}

export const AVAILABLE_LOCALES = [
  { code: 'en', label: 'English', nativeLabel: 'English' },
  { code: 'ru', label: 'Russian', nativeLabel: 'Русский' },
] as const satisfies ReadonlyArray<LocaleOption>

export type Locale = (typeof AVAILABLE_LOCALES)[number]['code']

// UI-local presentation enums (not API DTOs): device theme preference and the
// four visual states a screen surface can render.
export type Theme = 'light' | 'dark' | 'system'
export type Role = User['role']
export type DemoScenario = 'ready' | 'loading' | 'error' | 'empty'
