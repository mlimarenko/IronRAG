import type { LibraryGraphCoverageSummary, LibraryReadinessSummary } from './documents'

export type GraphNodeType = 'document' | 'entity' | 'topic'
export type GraphStatus =
  | 'empty'
  | 'building'
  | 'rebuilding'
  | 'ready'
  | 'partial'
  | 'failed'
  | 'stale'
export type GraphLayoutMode =
  | 'cloud'
  | 'circle'
  | 'rings'
  | 'lanes'
  | 'clusters'
  | 'islands'
  | 'spiral'
export type GraphConvergenceStatus = 'partial' | 'current' | 'degraded'
export type GraphMutationImpactScopeStatus =
  | 'pending'
  | 'targeted'
  | 'fallback_broad'
  | 'completed'
  | 'failed'
export type GraphMutationImpactScopeConfidence = 'high' | 'medium' | 'low'
export type GraphSummaryConfidenceStatus = 'strong' | 'partial' | 'weak' | 'conflicted'

export interface GraphNode {
  id: string
  canonicalKey?: string | null
  label: string
  nodeType: GraphNodeType
  secondaryLabel: string | null
  supportCount: number
  filteredArtifact: boolean
}

export interface GraphEdge {
  id: string
  canonicalKey?: string | null
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

export interface GraphSurfaceResponse {
  loading: boolean
  error: string | null
  canvasMode: GraphCanvasMode
  graphStatus: GraphStatus
  convergenceStatus: GraphConvergenceStatus | null
  graphGeneration: number
  graphGenerationState?: string | null
  nodeCount: number
  relationCount: number
  edgeCount: number
  hiddenNodeCount: number
  filteredArtifactCount: number | null
  lastBuiltAt: string | null
  readinessSummary: LibraryReadinessSummary | null
  graphCoverage: LibraryGraphCoverageSummary | null
  overlay: GraphOverlayState
  inspector: GraphInspectorState
  warning: string | null
  nodes: GraphNode[]
  edges: GraphEdge[]
  legend: GraphLegendItem[]
}

export interface GraphSearchHit {
  id: string
  label: string
  nodeType: GraphNodeType
  secondaryLabel: string | null
  preview?: string | null
}

export interface GraphExtractionRecoverySummary {
  status: 'clean' | 'recovered' | 'partial' | 'failed'
  parserRepairApplied: boolean
  secondPassApplied: boolean
  warning: string | null
}

export interface GraphMutationImpactScopeSummary {
  scopeStatus: GraphMutationImpactScopeStatus
  confidenceStatus: GraphMutationImpactScopeConfidence
  affectedNodeCount: number
  affectedRelationshipCount: number
  fallbackReason: string | null
}

export interface GraphCanonicalSummary {
  text: string
  confidenceStatus: GraphSummaryConfidenceStatus
  supportCount: number
  warning: string | null
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
  canonicalSummary: GraphCanonicalSummary | null
  reconciliationScope: GraphMutationImpactScopeSummary | null
  reconciliationStatus: string | null
  convergenceStatus: GraphConvergenceStatus | null
  pendingUpdateCount: number
  pendingDeleteCount: number
  activeProvenanceOnly: boolean
  filteredArtifactCount: number | null
  extractionRecovery: GraphExtractionRecoverySummary | null
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

export interface GraphDiagnostics {
  graphStatus: GraphStatus
  reconciliationStatus: string
  convergenceStatus: GraphConvergenceStatus | null
  graphGeneration: number
  nodeCount: number
  edgeCount: number
  graphFreshness: string
  rebuildBacklogCount: number
  graphSparseCount: number
  pendingUpdateCount: number
  pendingDeleteCount: number
  activeMutationScope: GraphMutationImpactScopeSummary | null
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

export type GraphCanvasMode = 'ready' | 'sparse' | 'building' | 'error' | 'empty'

export interface GraphOverlayState {
  searchQuery: string
  searchHits: GraphSearchHit[]
  nodeTypeFilter: GraphNodeType | ''
  activeLayout: GraphLayoutMode
  showFilteredArtifacts: boolean
  filteredArtifactCount: number
  nodeCount: number
  edgeCount: number
  showLegend: boolean
  showFilters: boolean
  zoomLevel: number
}

export interface GraphNodeSummary {
  id: string
  label: string
  nodeType: GraphNodeType
  supportCount: number
}

export interface GraphInspectorState {
  focusedNodeId: string | null
  loading: boolean
  error: string | null
  detail: GraphNodeDetail | null
}

export type GraphWorkspaceSurface = GraphSurfaceResponse

export function resolveDefaultGraphLayoutMode(
  nodeCount: number,
  edgeCount: number,
): GraphLayoutMode {
  if (nodeCount >= 220 || edgeCount >= 260) {
    return 'clusters'
  }
  if (nodeCount >= 120 || edgeCount >= 140) {
    return 'rings'
  }
  return 'cloud'
}
