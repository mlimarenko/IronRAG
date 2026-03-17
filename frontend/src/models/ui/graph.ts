export type GraphNodeType = 'document' | 'entity' | 'topic'
export type GraphStatus = 'empty' | 'building' | 'ready' | 'partial' | 'failed' | 'stale'
export type GraphQueryMode = 'document' | 'local' | 'global' | 'hybrid' | 'mix'
export type GraphLayoutMode = 'cloud' | 'rings' | 'lanes'
export type GraphGroundingStatus = 'grounded' | 'partial' | 'weak' | 'none'
export type GraphConvergenceStatus = 'partial' | 'current' | 'degraded'
export type QueryIntentCacheStatus = 'miss' | 'hit_fresh' | 'hit_stale_recomputed'
export type GraphRerankStatus = 'not_applicable' | 'applied' | 'skipped' | 'failed'
export type GraphContextAssemblyStatus =
  | 'document_only'
  | 'graph_only'
  | 'balanced_mixed'
  | 'mixed_skewed'

export interface QueryIntentKeywords {
  highLevel: string[]
  lowLevel: string[]
}

export interface QueryPlanningMetadata {
  requestedMode: GraphQueryMode
  plannedMode: GraphQueryMode
  intentCacheStatus: QueryIntentCacheStatus
  keywords: QueryIntentKeywords
  warnings: string[]
}

export interface RerankMetadata {
  status: GraphRerankStatus
  candidateCount: number
  reorderedCount: number | null
}

export interface ContextAssemblyMetadata {
  status: GraphContextAssemblyStatus
  warning: string | null
}

export interface GraphNode {
  id: string
  label: string
  nodeType: GraphNodeType
  secondaryLabel: string | null
  supportCount: number
  filteredArtifact: boolean
}

export interface GraphEdge {
  id: string
  source: string
  target: string
  relationType: string
  supportCount: number
  filteredArtifact: boolean
}

export interface GraphLegendItem {
  key: string
  label: string
}

export interface GraphAssistantMessage {
  id: string
  role: string
  content: string
  createdAt: string
  queryId: string | null
  mode: GraphQueryMode | null
  groundingStatus: GraphGroundingStatus | null
  provider: GraphAssistantProvider | null
  references: GraphAssistantReference[]
  planning: QueryPlanningMetadata | null
  rerank: RerankMetadata | null
  contextAssembly: ContextAssemblyMetadata | null
  warning: string | null
  warningKind: string | null
}

export interface GraphAssistantModeDescriptor {
  mode: GraphQueryMode
  labelKey: string
  shortDescriptionKey: string
  bestForKey: string
  cautionKey: string | null
  exampleQuestionKey: string
}

export interface GraphAssistantConfig {
  scopeHintKey: string
  defaultPromptKeys: string[]
  modes: GraphAssistantModeDescriptor[]
}

export interface GraphAssistantState {
  title: string
  subtitle: string
  prompts: string[]
  disclaimer: string
  sessionId: string | null
  messages: GraphAssistantMessage[]
}

export interface GraphSurfaceResponse {
  graphStatus: GraphStatus
  convergenceStatus: GraphConvergenceStatus | null
  projectionVersion: number
  nodeCount: number
  relationCount: number
  filteredArtifactCount: number | null
  lastBuiltAt: string | null
  warning: string | null
  nodes: GraphNode[]
  edges: GraphEdge[]
  legend: GraphLegendItem[]
  assistant: GraphAssistantState
}

export interface GraphSearchHit {
  id: string
  label: string
  nodeType: GraphNodeType
  secondaryLabel: string | null
}

export interface GraphNodeDetail {
  id: string
  label: string
  nodeType: GraphNodeType
  summary: string
  properties: [string, string][]
  relatedDocuments: GraphSearchHit[]
  connectedNodes: GraphSearchHit[]
  relatedEdges: GraphRelatedEdge[]
  evidence: GraphEvidence[]
  relationCount: number
  reconciliationStatus: string | null
  convergenceStatus: GraphConvergenceStatus | null
  pendingUpdateCount: number
  pendingDeleteCount: number
  activeProvenanceOnly: boolean
  filteredArtifactCount: number | null
  warning: string | null
}

export interface GraphRelatedEdge {
  id: string
  relationType: string
  otherNodeId: string
  otherNodeLabel: string
  supportCount: number
}

export interface GraphEvidence {
  id: string
  documentId: string | null
  documentLabel: string | null
  chunkId: string | null
  pageRef: string | null
  evidenceText: string
  confidenceScore: number | null
  createdAt: string
  activeProvenanceOnly: boolean
}

export interface GraphAssistantProvider {
  providerKind: string
  modelName: string
}

export interface GraphAssistantReference {
  kind: string
  referenceId: string
  excerpt: string | null
  rank: number
  score: number | null
}

export interface GraphAssistantAnswer {
  sessionId: string
  userMessageId: string
  assistantMessageId: string
  queryId: string
  answer: string
  references: string[]
  structuredReferences: GraphAssistantReference[]
  mode: GraphQueryMode
  groundingStatus: GraphGroundingStatus
  provider: GraphAssistantProvider
  planning: QueryPlanningMetadata
  rerank: RerankMetadata
  contextAssembly: ContextAssemblyMetadata
  warning: string | null
  warningKind: string | null
}

export interface GraphDiagnostics {
  graphStatus: GraphStatus
  reconciliationStatus: string
  convergenceStatus: GraphConvergenceStatus | null
  projectionVersion: number
  nodeCount: number
  edgeCount: number
  projectionFreshness: string
  rebuildBacklogCount: number
  readyNoGraphCount: number
  pendingUpdateCount: number
  pendingDeleteCount: number
  filteredArtifactCount: number | null
  filteredEmptyRelationCount: number | null
  filteredDegenerateLoopCount: number | null
  provenanceCoveragePercent: number | null
  lastBuiltAt: string | null
  lastErrorMessage: string | null
  lastMutationWarning: string | null
  activeProvenanceOnly: boolean
  blockers: string[]
  warning: string | null
  graphBackend: string
}
