import type {
  ContextAssemblyMetadata,
  GraphAssistantConfig,
  GraphAssistantAnswer,
  GraphAssistantProvider,
  GraphConvergenceStatus,
  GraphAssistantReference,
  GraphContextAssemblyStatus,
  GraphDiagnostics,
  GraphRerankStatus,
  GraphEvidence,
  GraphRelatedEdge,
  GraphNodeDetail,
  GraphQueryMode,
  QueryIntentCacheStatus,
  QueryPlanningMetadata,
  RerankMetadata,
  GraphSearchHit,
  GraphSurfaceResponse,
} from 'src/models/ui/graph'
import { apiHttp, unwrap } from './http'

interface RawGraphNode {
  id: string
  label: string
  node_type: 'document' | 'entity' | 'topic'
  secondary_label: string | null
  support_count: number
  filtered_artifact: boolean
}

interface RawGraphEdge {
  id: string
  source: string
  target: string
  relation_type: string
  support_count: number
  filtered_artifact: boolean
}

interface RawGraphLegendItem {
  key: string
  label: string
}

interface RawGraphAssistantMessage {
  id: string
  role: string
  content: string
  created_at: string
  query_id: string | null
  mode: 'document' | 'local' | 'global' | 'hybrid' | 'mix' | null
  grounding_status: 'grounded' | 'partial' | 'weak' | 'none' | null
  provider: RawGraphAssistantProvider | null
  references: RawGraphAssistantReference[]
  planning: RawQueryPlanningMetadata | null
  rerank: RawRerankMetadata | null
  context_assembly: RawContextAssemblyMetadata | null
  warning: string | null
  warning_kind: string | null
}

interface RawGraphAssistantProvider {
  provider_kind: string
  model_name: string
}

interface RawGraphAssistantReference {
  kind: string
  reference_id: string
  excerpt: string | null
  rank: number
  score: number | null
}

interface RawGraphAssistantModeDescriptor {
  mode: 'document' | 'local' | 'global' | 'hybrid' | 'mix'
  label_key: string
  short_description_key: string
  best_for_key: string
  caution_key: string | null
  example_question_key: string
}

interface RawGraphAssistantConfigResponse {
  scope_hint_key: string
  default_prompt_keys: string[]
  modes: RawGraphAssistantModeDescriptor[]
}

interface RawGraphSurfaceResponse {
  graph_status: 'empty' | 'building' | 'ready' | 'partial' | 'failed' | 'stale'
  convergence_status: GraphConvergenceStatus | null
  projection_version: number
  node_count: number
  relation_count: number
  filtered_artifact_count: number | null
  last_built_at: string | null
  warning: string | null
  nodes: RawGraphNode[]
  edges: RawGraphEdge[]
  legend: RawGraphLegendItem[]
  assistant: {
    title: string
    subtitle: string
    prompts: string[]
    disclaimer: string
    session_id: string | null
    messages: RawGraphAssistantMessage[]
  }
}

interface RawGraphNodeDetail {
  id: string
  label: string
  node_type: 'document' | 'entity' | 'topic'
  summary: string
  properties: [string, string][]
  related_documents: RawGraphNode[]
  connected_nodes: RawGraphNode[]
  related_edges: {
    id: string
    relation_type: string
    other_node_id: string
    other_node_label: string
    support_count: number
  }[]
  evidence: {
    id: string
    document_id: string | null
    document_label: string | null
    chunk_id: string | null
    page_ref: string | null
    evidence_text: string
    confidence_score: number | null
    created_at: string
    active_provenance_only: boolean
  }[]
  relation_count: number
  reconciliation_status: string | null
  convergence_status: GraphConvergenceStatus | null
  pending_update_count: number
  pending_delete_count: number
  active_provenance_only: boolean
  filtered_artifact_count: number | null
  warning: string | null
}

interface RawGraphAssistantAnswer {
  session_id: string
  user_message_id: string
  assistant_message_id: string
  query_id: string
  answer: string
  references: string[]
  structured_references: RawGraphAssistantReference[]
  mode: 'document' | 'local' | 'global' | 'hybrid' | 'mix'
  grounding_status: 'grounded' | 'partial' | 'weak' | 'none'
  provider: RawGraphAssistantProvider
  planning: RawQueryPlanningMetadata
  rerank: RawRerankMetadata
  context_assembly: RawContextAssemblyMetadata
  warning: string | null
  warning_kind: string | null
}

interface RawIntentKeywords {
  highLevel: string[]
  lowLevel: string[]
}

interface RawQueryPlanningMetadata {
  requestedMode: 'document' | 'local' | 'global' | 'hybrid' | 'mix'
  plannedMode: 'document' | 'local' | 'global' | 'hybrid' | 'mix'
  intentCacheStatus: QueryIntentCacheStatus
  keywords: RawIntentKeywords
  warnings: string[]
}

interface RawRerankMetadata {
  status: GraphRerankStatus
  candidateCount: number
  reorderedCount: number | null
}

interface RawContextAssemblyMetadata {
  status: GraphContextAssemblyStatus
  warning: string | null
}

interface RawGraphDiagnostics {
  graph_status: 'empty' | 'building' | 'ready' | 'partial' | 'failed' | 'stale'
  reconciliation_status: string
  convergence_status: GraphConvergenceStatus | null
  projection_version: number
  node_count: number
  edge_count: number
  projection_freshness: string
  rebuild_backlog_count: number
  ready_no_graph_count: number
  pending_update_count: number
  pending_delete_count: number
  filtered_artifact_count: number | null
  filtered_empty_relation_count: number | null
  filtered_degenerate_loop_count: number | null
  provenance_coverage_percent: number | null
  last_built_at: string | null
  last_error_message: string | null
  last_mutation_warning: string | null
  active_provenance_only: boolean
  blockers: string[]
  warning: string | null
  graph_backend: string
}

function mapNode(node: RawGraphNode) {
  return {
    id: node.id,
    label: node.label,
    nodeType: node.node_type,
    secondaryLabel: node.secondary_label,
    supportCount: node.support_count,
    filteredArtifact: node.filtered_artifact,
  }
}

function mapHit(hit: RawGraphNode): GraphSearchHit {
  return {
    id: hit.id,
    label: hit.label,
    nodeType: hit.node_type,
    secondaryLabel: hit.secondary_label,
  }
}

function mapProvider(provider: RawGraphAssistantProvider): GraphAssistantProvider {
  return {
    providerKind: provider.provider_kind,
    modelName: provider.model_name,
  }
}

function mapReference(reference: RawGraphAssistantReference): GraphAssistantReference {
  return {
    kind: reference.kind,
    referenceId: reference.reference_id,
    excerpt: reference.excerpt,
    rank: reference.rank,
    score: reference.score,
  }
}

function mapPlanningMetadata(metadata: RawQueryPlanningMetadata): QueryPlanningMetadata {
  return {
    requestedMode: metadata.requestedMode,
    plannedMode: metadata.plannedMode,
    intentCacheStatus: metadata.intentCacheStatus,
    keywords: {
      highLevel: metadata.keywords.highLevel,
      lowLevel: metadata.keywords.lowLevel,
    },
    warnings: metadata.warnings,
  }
}

function mapRerankMetadata(metadata: RawRerankMetadata): RerankMetadata {
  return {
    status: metadata.status,
    candidateCount: metadata.candidateCount,
    reorderedCount: metadata.reorderedCount,
  }
}

function mapContextAssemblyMetadata(metadata: RawContextAssemblyMetadata): ContextAssemblyMetadata {
  return {
    status: metadata.status,
    warning: metadata.warning,
  }
}

function mapAssistantModeDescriptor(mode: RawGraphAssistantModeDescriptor) {
  return {
    mode: mode.mode,
    labelKey: mode.label_key,
    shortDescriptionKey: mode.short_description_key,
    bestForKey: mode.best_for_key,
    cautionKey: mode.caution_key,
    exampleQuestionKey: mode.example_question_key,
  }
}

export async function fetchGraphAssistantConfig(libraryId: string): Promise<GraphAssistantConfig> {
  const response = await unwrap(
    apiHttp.get<RawGraphAssistantConfigResponse>(`/ui/libraries/${libraryId}/graph/assistant/config`),
  )
  return {
    scopeHintKey: response.scope_hint_key,
    defaultPromptKeys: response.default_prompt_keys,
    modes: response.modes.map(mapAssistantModeDescriptor),
  }
}

export async function fetchGraphSurface(options?: {
  includeFiltered?: boolean
}): Promise<GraphSurfaceResponse> {
  const response = await unwrap(
    apiHttp.get<RawGraphSurfaceResponse>('/ui/graph/surface', {
      params: {
        include_filtered: options?.includeFiltered ? 'true' : undefined,
      },
    }),
  )
  return {
    graphStatus: response.graph_status,
    convergenceStatus: response.convergence_status,
    projectionVersion: response.projection_version,
    nodeCount: response.node_count,
    relationCount: response.relation_count,
    filteredArtifactCount: response.filtered_artifact_count,
    lastBuiltAt: response.last_built_at,
    warning: response.warning,
    nodes: response.nodes.map(mapNode),
    edges: response.edges.map((edge) => ({
      id: edge.id,
      source: edge.source,
      target: edge.target,
      relationType: edge.relation_type,
      supportCount: edge.support_count,
      filteredArtifact: edge.filtered_artifact,
    })),
    legend: response.legend,
    assistant: {
      title: response.assistant.title,
      subtitle: response.assistant.subtitle,
      prompts: response.assistant.prompts,
      disclaimer: response.assistant.disclaimer,
      sessionId: response.assistant.session_id,
      messages: response.assistant.messages.map((message) => ({
        id: message.id,
        role: message.role,
        content: message.content,
        createdAt: message.created_at,
        queryId: message.query_id,
        mode: message.mode,
        groundingStatus: message.grounding_status,
        provider: message.provider ? mapProvider(message.provider) : null,
        references: message.references.map(mapReference),
        planning: message.planning ? mapPlanningMetadata(message.planning) : null,
        rerank: message.rerank ? mapRerankMetadata(message.rerank) : null,
        contextAssembly: message.context_assembly
          ? mapContextAssemblyMetadata(message.context_assembly)
          : null,
        warning: message.warning,
        warningKind: message.warning_kind,
      })),
    },
  }
}

export async function fetchGraphDiagnostics(): Promise<GraphDiagnostics> {
  const response = await unwrap(apiHttp.get<RawGraphDiagnostics>('/ui/graph/diagnostics'))
  return {
    graphStatus: response.graph_status,
    reconciliationStatus: response.reconciliation_status,
    convergenceStatus: response.convergence_status,
    projectionVersion: response.projection_version,
    nodeCount: response.node_count,
    edgeCount: response.edge_count,
    projectionFreshness: response.projection_freshness,
    rebuildBacklogCount: response.rebuild_backlog_count,
    readyNoGraphCount: response.ready_no_graph_count,
    pendingUpdateCount: response.pending_update_count,
    pendingDeleteCount: response.pending_delete_count,
    filteredArtifactCount: response.filtered_artifact_count,
    filteredEmptyRelationCount: response.filtered_empty_relation_count,
    filteredDegenerateLoopCount: response.filtered_degenerate_loop_count,
    provenanceCoveragePercent: response.provenance_coverage_percent,
    lastBuiltAt: response.last_built_at,
    lastErrorMessage: response.last_error_message,
    lastMutationWarning: response.last_mutation_warning,
    activeProvenanceOnly: response.active_provenance_only,
    blockers: response.blockers,
    warning: response.warning,
    graphBackend: response.graph_backend,
  }
}

export async function searchGraphNodes(
  query: string,
  options?: { includeFiltered?: boolean },
): Promise<GraphSearchHit[]> {
  return (
    await unwrap(
      apiHttp.get<RawGraphNode[]>('/ui/graph/search', {
        params: {
          q: query,
          include_filtered: options?.includeFiltered ? 'true' : undefined,
        },
      }),
    )
  ).map(mapHit)
}

export async function fetchGraphNodeDetail(
  id: string,
  options?: { includeFiltered?: boolean },
): Promise<GraphNodeDetail> {
  const response = await unwrap(
    apiHttp.get<RawGraphNodeDetail>(`/ui/graph/nodes/${id}`, {
      params: {
        include_filtered: options?.includeFiltered ? 'true' : undefined,
      },
    }),
  )
  return {
    id: response.id,
    label: response.label,
    nodeType: response.node_type,
    summary: response.summary,
    properties: response.properties,
    relatedDocuments: response.related_documents.map(mapHit),
    connectedNodes: response.connected_nodes.map(mapHit),
    relatedEdges: response.related_edges.map(
      (edge): GraphRelatedEdge => ({
        id: edge.id,
        relationType: edge.relation_type,
        otherNodeId: edge.other_node_id,
        otherNodeLabel: edge.other_node_label,
        supportCount: edge.support_count,
      }),
    ),
    evidence: response.evidence.map(
      (evidence): GraphEvidence => ({
        id: evidence.id,
        documentId: evidence.document_id,
        documentLabel: evidence.document_label,
        chunkId: evidence.chunk_id,
        pageRef: evidence.page_ref,
        evidenceText: evidence.evidence_text,
        confidenceScore: evidence.confidence_score,
        createdAt: evidence.created_at,
        activeProvenanceOnly: evidence.active_provenance_only,
      }),
    ),
    relationCount: response.relation_count,
    reconciliationStatus: response.reconciliation_status,
    convergenceStatus: response.convergence_status,
    pendingUpdateCount: response.pending_update_count,
    pendingDeleteCount: response.pending_delete_count,
    activeProvenanceOnly: response.active_provenance_only,
    filteredArtifactCount: response.filtered_artifact_count,
    warning: response.warning,
  }
}

export async function askGraphAssistant(
  question: string,
  sessionId: string | null,
  nodeId: string | null,
  mode: GraphQueryMode,
): Promise<GraphAssistantAnswer> {
  const response = await unwrap(
    apiHttp.post<RawGraphAssistantAnswer>('/ui/graph/ask', {
      question,
      session_id: sessionId,
      node_id: nodeId,
      mode,
    }),
  )
  return {
    sessionId: response.session_id,
    userMessageId: response.user_message_id,
    assistantMessageId: response.assistant_message_id,
    queryId: response.query_id,
    answer: response.answer,
    references: response.references,
    structuredReferences: response.structured_references.map(mapReference),
    mode: response.mode,
    groundingStatus: response.grounding_status,
    provider: mapProvider(response.provider),
    planning: mapPlanningMetadata(response.planning),
    rerank: mapRerankMetadata(response.rerank),
    contextAssembly: mapContextAssemblyMetadata(response.context_assembly),
    warning: response.warning,
    warningKind: response.warning_kind,
  }
}
