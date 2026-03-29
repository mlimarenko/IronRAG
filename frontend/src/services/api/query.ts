import { unwrap, apiHttp } from './http'

export type QueryMode = 'document' | 'local' | 'global' | 'hybrid' | 'mix'

export interface QuerySession {
  id: string
  workspaceId: string
  libraryId: string
  createdByPrincipalId: string | null
  title: string | null
  conversationState: string
  createdAt: string
  updatedAt: string
}

export interface QueryTurn {
  id: string
  conversationId: string
  turnIndex: number
  turnKind: string
  authorPrincipalId: string | null
  contentText: string
  executionId: string | null
  createdAt: string
}

export interface QueryExecution {
  id: string
  workspaceId: string
  libraryId: string
  conversationId: string
  contextBundleId: string | null
  requestTurnId: string | null
  responseTurnId: string | null
  bindingId: string | null
  executionState: string
  queryText: string
  failureCode: string | null
  startedAt: string
  completedAt: string | null
}

export interface QueryChunkReference {
  executionId: string
  chunkId: string
  rank: number
  score: number
}

export interface QueryEntityReference {
  executionId: string
  nodeId: string
  rank: number
  score: number
}

export interface QueryRelationReference {
  executionId: string
  edgeId: string
  rank: number
  score: number
}

export interface QuerySessionDetail {
  session: QuerySession
  turns: QueryTurn[]
  executions: QueryExecution[]
}

export interface QueryExecutionDetail {
  contextBundleId: string
  execution: QueryExecution
  requestTurn: QueryTurn | null
  responseTurn: QueryTurn | null
  chunkReferences: QueryChunkReference[]
  entityReferences: QueryEntityReference[]
  relationReferences: QueryRelationReference[]
}

export interface QueryTurnExecutionResult {
  contextBundleId: string
  session: QuerySession
  requestTurn: QueryTurn
  responseTurn: QueryTurn | null
  execution: QueryExecution
}

export interface KnowledgeContextBundle {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  bundleId: string
  workspaceId: string
  libraryId: string
  queryExecutionId: string | null
  bundleState: string
  bundleStrategy: string
  requestedMode: string
  resolvedMode: string
  freshnessSnapshot: Record<string, unknown>
  candidateSummary: Record<string, unknown>
  assemblyDiagnostics: Record<string, unknown>
  createdAt: string
  updatedAt: string
}

export interface KnowledgeRetrievalTrace {
  key: string
  traceId: string
  workspaceId: string
  libraryId: string
  queryExecutionId: string | null
  bundleId: string
  traceState: string
  retrievalStrategy: string
  candidateCounts: Record<string, unknown>
  droppedReasons: Record<string, unknown>
  timingBreakdown: Record<string, unknown>
  diagnosticsJson: Record<string, unknown>
  createdAt: string
  updatedAt: string
}

export interface KnowledgeBundleChunkReference {
  key: string
  bundleId: string
  chunkId: string
  rank: number
  score: number
  inclusionReason: string | null
  createdAt: string
}

export interface KnowledgeBundleEntityReference {
  key: string
  bundleId: string
  entityId: string
  rank: number
  score: number
  inclusionReason: string | null
  createdAt: string
}

export interface KnowledgeBundleRelationReference {
  key: string
  bundleId: string
  relationId: string
  rank: number
  score: number
  inclusionReason: string | null
  createdAt: string
}

export interface KnowledgeBundleEvidenceReference {
  key: string
  bundleId: string
  evidenceId: string
  rank: number
  score: number
  inclusionReason: string | null
  createdAt: string
}

export interface KnowledgeContextBundleDetail {
  bundle: KnowledgeContextBundle
  traces: KnowledgeRetrievalTrace[]
  chunkReferences: KnowledgeBundleChunkReference[]
  entityReferences: KnowledgeBundleEntityReference[]
  relationReferences: KnowledgeBundleRelationReference[]
  evidenceReferences: KnowledgeBundleEvidenceReference[]
}

export interface CreateQuerySessionPayload {
  workspaceId: string
  libraryId: string
  title?: string | null
}

export interface ExecuteQueryTurnPayload {
  contentText: string
  mode: QueryMode
  topK?: number
  includeDebug?: boolean
}

export async function listQuerySessions(libraryId: string): Promise<QuerySession[]> {
  return unwrap(apiHttp.get<QuerySession[]>('/query/sessions', { params: { libraryId } }))
}

export async function createQuerySession(
  payload: CreateQuerySessionPayload,
): Promise<QuerySession> {
  return unwrap(apiHttp.post<QuerySession>('/query/sessions', payload))
}

export async function fetchQuerySessionDetail(sessionId: string): Promise<QuerySessionDetail> {
  return unwrap(apiHttp.get<QuerySessionDetail>(`/query/sessions/${sessionId}`))
}

export async function executeQueryTurn(
  sessionId: string,
  payload: ExecuteQueryTurnPayload,
): Promise<QueryTurnExecutionResult> {
  return unwrap(apiHttp.post<QueryTurnExecutionResult>(`/query/sessions/${sessionId}/turns`, payload))
}

export async function fetchQueryExecutionDetail(
  executionId: string,
): Promise<QueryExecutionDetail> {
  return unwrap(apiHttp.get<QueryExecutionDetail>(`/query/executions/${executionId}`))
}

export async function fetchKnowledgeContextBundle(
  bundleId: string,
): Promise<KnowledgeContextBundleDetail> {
  return unwrap(apiHttp.get<KnowledgeContextBundleDetail>(`/knowledge/context-bundles/${bundleId}`))
}
