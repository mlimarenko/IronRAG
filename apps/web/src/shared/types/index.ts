import type {
  AiBindingPurpose,
  BootstrapBindingPurpose,
  DocumentReadiness as GeneratedDocumentReadiness,
  DocumentStatus as GeneratedDocumentStatus,
  GraphStatus as GeneratedGraphStatus,
} from "@/shared/api/generated";

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
  missingBindingPurposes: BootstrapBindingPurpose[];
}

export type AIPurpose = AiBindingPurpose;

/**
 * Canonical document status enum — mirrors the backend `derived_status`
 * CASE expression in `list_document_page_rows`. These are the only
 * values the list API will ever emit; the UI stays 1:1 with the server
 * so nothing has to re-derive buckets client-side.
 */
export type DocumentStatus = GeneratedDocumentStatus;
export type DocumentReadiness = GeneratedDocumentReadiness;
type SourceAccessKind = "stored_document" | "external_url";

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

// Graph types
export type GraphStatus = GeneratedGraphStatus;
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

type AssistantStage = "planning" | "grounding" | "response";

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

interface FileAttachment {
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

interface SegmentReference {
  documentId: string;
  documentName: string;
  documentTitle: string | null;
  sourceUri: string | null;
  sourceAccess: SourceAccess | null;
  segmentOrdinal: number;
  excerpt: string;
  relevance: number;
}

interface FactReference {
  factKind: string;
  value: string;
  confidence: number;
  documentName: string;
}

interface EntityReference {
  entityId: string;
  label: string;
  type: string;
  relevance: number;
}

interface RelationReference {
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

interface RuntimeSummary {
  totalSegments: number;
  totalFacts: number;
  totalEntities: number;
  totalRelations: number;
  stages: RuntimeStageSummary[];
  policyInterventions: PolicyIntervention[];
}

interface RuntimeStageSummary {
  stage: string;
  durationMs: number;
  itemCount: number;
}

interface PolicyIntervention {
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

interface TokenIssuer {
  id: string;
  displayLabel: string;
}

interface TokenScopeWorkspace {
  id: string;
  displayName: string;
}

interface TokenScopeLibrary {
  id: string;
  workspaceId: string;
  displayName: string;
}

interface TokenScope {
  kind: "system" | "workspace" | "library";
  workspace?: TokenScopeWorkspace;
  libraries: TokenScopeLibrary[];
}

interface TokenGrant {
  resourceKind: string;
  resourceId: string;
  permission: string;
  workspace?: TokenScopeWorkspace;
  library?: TokenScopeLibrary;
}

export type AIScopeKind = "instance" | "workspace" | "library";

export type AIProviderCapabilities = Record<string, unknown>;
export type AIProviderRuntime = Record<string, unknown>;
export type AIProviderUiHints = Record<string, unknown>;
export type AIProviderBaseUrlMode = "fixed" | "required" | "optional";
export type AIProviderCredentialValidationMode = "chat_round_trip" | "model_list" | "none";

export interface AIProviderCredentialPolicy {
  apiKeyRequired: boolean;
  baseUrlRequired: boolean;
  baseUrlMode: AIProviderBaseUrlMode;
  validationMode: AIProviderCredentialValidationMode;
}

export interface AIProviderBaseUrlPolicy {
  allowOverride: boolean;
  requireHttps: boolean;
  allowPrivateNetwork: boolean;
  trimSuffixes: string[];
}

export interface AIProviderModelDiscovery {
  mode: "shared" | "credential" | "unsupported";
  paths: Array<{
    capabilityKind: string;
    path: string;
  }>;
}

export interface AIProvider {
  id: string;
  displayName: string;
  kind: string;
  apiStyle: string;
  lifecycleState: "active" | "deprecated" | "preview";
  defaultBaseUrl?: string;
  apiKeyRequired: boolean;
  baseUrlRequired: boolean;
  credentialPolicy: AIProviderCredentialPolicy;
  baseUrlPolicy: AIProviderBaseUrlPolicy;
  modelDiscovery: AIProviderModelDiscovery;
  capabilities: AIProviderCapabilities;
  runtime: AIProviderRuntime;
  uiHints: AIProviderUiHints;
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
  provider?: AIProvider;
  label: string;
  state: "active" | "invalid" | "revoked" | "unchecked";
  createdAt: string;
  updatedAt: string;
  baseUrl?: string;
  apiKeySummary: string;
}

type AIModelAvailabilityState = "available" | "unavailable" | "unknown";

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

interface AuditAssistantModel {
  providerKind: string;
  modelName: string;
}

interface AuditAssistantCall {
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

export type Locale = string;

interface LocaleOption {
  code: string;
  label: string;
  nativeLabel: string;
}

export const AVAILABLE_LOCALES: LocaleOption[] = [
  { code: "en", label: "English", nativeLabel: "English" },
  { code: "ru", label: "Russian", nativeLabel: "Русский" },
];
