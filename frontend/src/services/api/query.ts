import {
  ApiClientError,
  apiHttp,
  resolveApiPath,
  unwrap,
} from './http'

export type QueryTurnStreamStage = 'retrieving' | 'grounding' | 'answering'

type RawRow = Record<string, unknown>

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
  topK?: number
  includeDebug?: boolean
}

export interface ExecuteQueryTurnStreamHandlers {
  onStage?: (stage: QueryTurnStreamStage) => void
  onAnswerDelta?: (delta: string) => void
}

function normalizeString(value: unknown): string {
  return typeof value === 'string' ? value : String(value ?? '')
}

function normalizeNullableString(value: unknown): string | null {
  if (value === null || value === undefined || value === '') {
    return null
  }
  return String(value)
}

function normalizeNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : 0
}

function normalizeBooleanRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : {}
}

function normalizeQuerySessionRow(row: RawRow): QuerySession {
  return {
    id: normalizeString(row.id),
    workspaceId: normalizeString(row.workspaceId ?? row.workspace_id),
    libraryId: normalizeString(row.libraryId ?? row.library_id),
    createdByPrincipalId: normalizeNullableString(
      row.createdByPrincipalId ?? row.created_by_principal_id,
    ),
    title: normalizeNullableString(row.title),
    conversationState: normalizeString(row.conversationState ?? row.conversation_state),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
    updatedAt: normalizeString(row.updatedAt ?? row.updated_at),
  }
}

function normalizeQueryTurnRow(row: RawRow): QueryTurn {
  return {
    id: normalizeString(row.id),
    conversationId: normalizeString(row.conversationId ?? row.conversation_id),
    turnIndex: normalizeNumber(row.turnIndex ?? row.turn_index),
    turnKind: normalizeString(row.turnKind ?? row.turn_kind),
    authorPrincipalId: normalizeNullableString(
      row.authorPrincipalId ?? row.author_principal_id,
    ),
    contentText: normalizeString(row.contentText ?? row.content_text),
    executionId: normalizeNullableString(row.executionId ?? row.execution_id),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
  }
}

function normalizeQueryExecutionRow(row: RawRow): QueryExecution {
  return {
    id: normalizeString(row.id),
    workspaceId: normalizeString(row.workspaceId ?? row.workspace_id),
    libraryId: normalizeString(row.libraryId ?? row.library_id),
    conversationId: normalizeString(row.conversationId ?? row.conversation_id),
    contextBundleId: normalizeNullableString(row.contextBundleId ?? row.context_bundle_id),
    requestTurnId: normalizeNullableString(row.requestTurnId ?? row.request_turn_id),
    responseTurnId: normalizeNullableString(row.responseTurnId ?? row.response_turn_id),
    bindingId: normalizeNullableString(row.bindingId),
    executionState: normalizeString(row.executionState ?? row.execution_state),
    queryText: normalizeString(row.queryText ?? row.query_text),
    failureCode: normalizeNullableString(row.failureCode ?? row.failure_code),
    startedAt: normalizeString(row.startedAt ?? row.started_at),
    completedAt: normalizeNullableString(row.completedAt ?? row.completed_at),
  }
}

function normalizeQueryChunkReference(row: RawRow): QueryChunkReference {
  return {
    executionId: normalizeString(row.executionId ?? row.execution_id),
    chunkId: normalizeString(row.chunkId ?? row.chunk_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
  }
}

function normalizeQueryEntityReference(row: RawRow): QueryEntityReference {
  return {
    executionId: normalizeString(row.executionId ?? row.execution_id),
    nodeId: normalizeString(row.nodeId ?? row.node_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
  }
}

function normalizeQueryRelationReference(row: RawRow): QueryRelationReference {
  return {
    executionId: normalizeString(row.executionId ?? row.execution_id),
    edgeId: normalizeString(row.edgeId ?? row.edge_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
  }
}

function normalizeKnowledgeContextBundle(row: RawRow): KnowledgeContextBundle {
  return {
    key: normalizeString(row.key),
    arangoId: normalizeNullableString(row.arangoId ?? row.arango_id),
    arangoRev: normalizeNullableString(row.arangoRev ?? row.arango_rev),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    workspaceId: normalizeString(row.workspaceId ?? row.workspace_id),
    libraryId: normalizeString(row.libraryId ?? row.library_id),
    queryExecutionId: normalizeNullableString(
      row.queryExecutionId ?? row.query_execution_id,
    ),
    bundleState: normalizeString(row.bundleState ?? row.bundle_state),
    bundleStrategy: normalizeString(row.bundleStrategy ?? row.bundle_strategy),
    requestedMode: normalizeString(row.requestedMode ?? row.requested_mode),
    resolvedMode: normalizeString(row.resolvedMode ?? row.resolved_mode),
    freshnessSnapshot: normalizeBooleanRecord(
      row.freshnessSnapshot ?? row.freshness_snapshot,
    ),
    candidateSummary: normalizeBooleanRecord(
      row.candidateSummary ?? row.candidate_summary,
    ),
    assemblyDiagnostics: normalizeBooleanRecord(
      row.assemblyDiagnostics ?? row.assembly_diagnostics,
    ),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
    updatedAt: normalizeString(row.updatedAt ?? row.updated_at),
  }
}

function normalizeKnowledgeRetrievalTrace(row: RawRow): KnowledgeRetrievalTrace {
  return {
    key: normalizeString(row.key),
    traceId: normalizeString(row.traceId ?? row.trace_id),
    workspaceId: normalizeString(row.workspaceId ?? row.workspace_id),
    libraryId: normalizeString(row.libraryId ?? row.library_id),
    queryExecutionId: normalizeNullableString(
      row.queryExecutionId ?? row.query_execution_id,
    ),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    traceState: normalizeString(row.traceState ?? row.trace_state),
    retrievalStrategy: normalizeString(row.retrievalStrategy ?? row.retrieval_strategy),
    candidateCounts: normalizeBooleanRecord(row.candidateCounts ?? row.candidate_counts),
    droppedReasons: normalizeBooleanRecord(row.droppedReasons ?? row.dropped_reasons),
    timingBreakdown: normalizeBooleanRecord(row.timingBreakdown ?? row.timing_breakdown),
    diagnosticsJson: normalizeBooleanRecord(row.diagnosticsJson ?? row.diagnostics_json),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
    updatedAt: normalizeString(row.updatedAt ?? row.updated_at),
  }
}

function normalizeKnowledgeBundleChunkReference(
  row: RawRow,
): KnowledgeBundleChunkReference {
  return {
    key: normalizeString(row.key),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    chunkId: normalizeString(row.chunkId ?? row.chunk_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
    inclusionReason: normalizeNullableString(row.inclusionReason ?? row.inclusion_reason),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
  }
}

function normalizeKnowledgeBundleEntityReference(
  row: RawRow,
): KnowledgeBundleEntityReference {
  return {
    key: normalizeString(row.key),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    entityId: normalizeString(row.entityId ?? row.entity_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
    inclusionReason: normalizeNullableString(row.inclusionReason ?? row.inclusion_reason),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
  }
}

function normalizeKnowledgeBundleRelationReference(
  row: RawRow,
): KnowledgeBundleRelationReference {
  return {
    key: normalizeString(row.key),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    relationId: normalizeString(row.relationId ?? row.relation_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
    inclusionReason: normalizeNullableString(row.inclusionReason ?? row.inclusion_reason),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
  }
}

function normalizeKnowledgeBundleEvidenceReference(
  row: RawRow,
): KnowledgeBundleEvidenceReference {
  return {
    key: normalizeString(row.key),
    bundleId: normalizeString(row.bundleId ?? row.bundle_id),
    evidenceId: normalizeString(row.evidenceId ?? row.evidence_id),
    rank: normalizeNumber(row.rank),
    score: normalizeNumber(row.score),
    inclusionReason: normalizeNullableString(row.inclusionReason ?? row.inclusion_reason),
    createdAt: normalizeString(row.createdAt ?? row.created_at),
  }
}

export async function listQuerySessions(libraryId: string): Promise<QuerySession[]> {
  const payload = await unwrap(apiHttp.get<RawRow[]>('/query/sessions', { params: { libraryId } }))
  return payload.map((row) => normalizeQuerySessionRow(row))
}

export async function createQuerySession(
  payload: CreateQuerySessionPayload,
): Promise<QuerySession> {
  const response = await unwrap(apiHttp.post<RawRow>('/query/sessions', payload))
  return normalizeQuerySessionRow(response)
}

export async function fetchQuerySessionDetail(sessionId: string): Promise<QuerySessionDetail> {
  const payload = await unwrap(
    apiHttp.get<{
      session: RawRow
      turns: RawRow[]
      executions: RawRow[]
    }>(`/query/sessions/${sessionId}`),
  )
  return {
    session: normalizeQuerySessionRow(payload.session),
    turns: payload.turns.map((row) => normalizeQueryTurnRow(row)),
    executions: payload.executions.map((row) => normalizeQueryExecutionRow(row)),
  }
}

export async function executeQueryTurn(
  sessionId: string,
  payload: ExecuteQueryTurnPayload,
  handlers: ExecuteQueryTurnStreamHandlers = {},
): Promise<QueryTurnExecutionResult> {
  const response = await fetch(resolveApiPath(`/query/sessions/${sessionId}/turns`), {
    method: 'POST',
    credentials: 'include',
    headers: {
      Accept: 'text/event-stream, application/json',
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(payload),
  })

  if (!response.ok) {
    throw await buildQueryTurnClientError(response)
  }

  const contentType = response.headers.get('content-type') ?? ''
  if (!contentType.includes('text/event-stream')) {
    const jsonResponse = (await response.json()) as {
      contextBundleId?: string | null
      context_bundle_id?: string | null
      session: RawRow
      requestTurn?: RawRow
      request_turn?: RawRow
      responseTurn?: RawRow | null
      response_turn?: RawRow | null
      execution: RawRow
    }
    return normalizeQueryTurnExecutionResult(jsonResponse)
  }

  return executeQueryTurnStream(response, handlers)
}

async function executeQueryTurnStream(
  response: Response,
  handlers: ExecuteQueryTurnStreamHandlers,
): Promise<QueryTurnExecutionResult> {
  const reader = response.body?.getReader()
  if (!reader) {
    throw new ApiClientError('Streaming response body is missing', 500, 'internal')
  }
  const decoder = new TextDecoder()
  let buffer = ''
  let completedResult: QueryTurnExecutionResult | null = null

  while (true) {
    const { done, value } = await reader.read()
    if (done) {
      break
    }
    buffer += decoder.decode(value, { stream: true }).replace(/\r\n/g, '\n')

    let frameBoundary = buffer.indexOf('\n\n')
    while (frameBoundary >= 0) {
      const frame = buffer.slice(0, frameBoundary)
      buffer = buffer.slice(frameBoundary + 2)
      await consumeQueryTurnStreamFrame(frame, handlers, (result) => {
        completedResult = result
      })
      frameBoundary = buffer.indexOf('\n\n')
    }
  }

  const flushed = decoder.decode()
  if (flushed) {
    buffer += flushed.replace(/\r\n/g, '\n')
  }
  if (buffer.trim()) {
    await consumeQueryTurnStreamFrame(buffer, handlers, (result) => {
      completedResult = result
    })
  }

  if (!completedResult) {
    throw new ApiClientError('Streaming query turn ended without a completed result', 500, 'internal')
  }
  return completedResult
}

async function consumeQueryTurnStreamFrame(
  frame: string,
  handlers: ExecuteQueryTurnStreamHandlers,
  setCompletedResult: (result: QueryTurnExecutionResult) => void,
): Promise<void> {
  if (!frame.trim() || frame.startsWith(':')) {
    return
  }

  let eventName = 'message'
  const dataLines: string[] = []
  for (const rawLine of frame.split('\n')) {
    const line = rawLine.trimEnd()
    if (line.startsWith('event:')) {
      eventName = line.slice(6).trim()
      continue
    }
    if (line.startsWith('data:')) {
      dataLines.push(line.slice(5).trimStart())
    }
  }

  if (dataLines.length === 0) {
    return
  }

  const payload = JSON.parse(dataLines.join('\n')) as Record<string, unknown>
  if (eventName === 'status') {
    const stage = normalizeString(payload.stage) as QueryTurnStreamStage
    handlers.onStage?.(stage)
    return
  }

  if (eventName === 'delta') {
    handlers.onAnswerDelta?.(normalizeString(payload.delta))
    return
  }

  if (eventName === 'completed') {
    setCompletedResult(
      normalizeQueryTurnExecutionResult(payload as Record<string, unknown> as {
        contextBundleId?: string | null
        context_bundle_id?: string | null
        session: RawRow
        requestTurn?: RawRow
        request_turn?: RawRow
        responseTurn?: RawRow | null
        response_turn?: RawRow | null
        execution: RawRow
      }),
    )
    return
  }

  if (eventName === 'error') {
    throw new ApiClientError(
      normalizeString(payload.error),
      500,
      normalizeNullableString(payload.errorKind ?? payload.error_kind),
    )
  }
}

function normalizeQueryTurnExecutionResult(response: {
  contextBundleId?: string | null
  context_bundle_id?: string | null
  session: RawRow
  requestTurn?: RawRow
  request_turn?: RawRow
  responseTurn?: RawRow | null
  response_turn?: RawRow | null
  execution: RawRow
}): QueryTurnExecutionResult {
  return {
    contextBundleId: normalizeString(
      response.contextBundleId ?? response.context_bundle_id,
    ),
    session: normalizeQuerySessionRow(response.session),
    requestTurn: normalizeQueryTurnRow(
      (response.requestTurn ?? response.request_turn) as RawRow,
    ),
    responseTurn: response.responseTurn || response.response_turn
      ? normalizeQueryTurnRow(
          ((response.responseTurn ?? response.response_turn) as RawRow),
        )
      : null,
    execution: normalizeQueryExecutionRow(response.execution),
  }
}

async function buildQueryTurnClientError(response: Response): Promise<ApiClientError> {
  const contentType = response.headers.get('content-type') ?? ''
  if (contentType.includes('application/json')) {
    const payload = (await response.json().catch(() => null)) as
      | {
          error?: string
          errorKind?: string | null
          error_kind?: string | null
          details?: unknown
          requestId?: string | null
          request_id?: string | null
        }
      | null
    return new ApiClientError(
      normalizeString(payload?.error ?? response.statusText ?? 'Request failed'),
      response.status,
      normalizeNullableString(payload?.errorKind ?? payload?.error_kind),
      payload?.details ?? null,
      normalizeNullableString(payload?.requestId ?? payload?.request_id),
    )
  }
  return new ApiClientError(response.statusText || 'Request failed', response.status)
}

export async function fetchQueryExecutionDetail(
  executionId: string,
): Promise<QueryExecutionDetail> {
  const payload = await unwrap(
    apiHttp.get<{
      contextBundleId?: string | null
      context_bundle_id?: string | null
      execution: RawRow
      requestTurn?: RawRow | null
      request_turn?: RawRow | null
      responseTurn?: RawRow | null
      response_turn?: RawRow | null
      chunkReferences?: RawRow[]
      chunk_references?: RawRow[]
      entityReferences?: RawRow[]
      entity_references?: RawRow[]
      relationReferences?: RawRow[]
      relation_references?: RawRow[]
    }>(`/query/executions/${executionId}`),
  )
  return {
    contextBundleId: normalizeString(
      payload.contextBundleId ?? payload.context_bundle_id,
    ),
    execution: normalizeQueryExecutionRow(payload.execution),
    requestTurn: payload.requestTurn || payload.request_turn
      ? normalizeQueryTurnRow((payload.requestTurn ?? payload.request_turn) as RawRow)
      : null,
    responseTurn: payload.responseTurn || payload.response_turn
      ? normalizeQueryTurnRow((payload.responseTurn ?? payload.response_turn) as RawRow)
      : null,
    chunkReferences: (payload.chunkReferences ?? payload.chunk_references ?? []).map((row) =>
      normalizeQueryChunkReference(row),
    ),
    entityReferences: (payload.entityReferences ?? payload.entity_references ?? []).map((row) =>
      normalizeQueryEntityReference(row),
    ),
    relationReferences: (
      payload.relationReferences ?? payload.relation_references ?? []
    ).map((row) => normalizeQueryRelationReference(row)),
  }
}

export async function fetchKnowledgeContextBundle(
  bundleId: string,
): Promise<KnowledgeContextBundleDetail> {
  const payload = await unwrap(
    apiHttp.get<{
      bundle: RawRow
      traces: RawRow[]
      chunkReferences?: RawRow[]
      chunk_references?: RawRow[]
      entityReferences?: RawRow[]
      entity_references?: RawRow[]
      relationReferences?: RawRow[]
      relation_references?: RawRow[]
      evidenceReferences?: RawRow[]
      evidence_references?: RawRow[]
    }>(`/knowledge/context-bundles/${bundleId}`),
  )
  return {
    bundle: normalizeKnowledgeContextBundle(payload.bundle),
    traces: payload.traces.map((row) => normalizeKnowledgeRetrievalTrace(row)),
    chunkReferences: (payload.chunkReferences ?? payload.chunk_references ?? []).map((row) =>
      normalizeKnowledgeBundleChunkReference(row),
    ),
    entityReferences: (payload.entityReferences ?? payload.entity_references ?? []).map((row) =>
      normalizeKnowledgeBundleEntityReference(row),
    ),
    relationReferences: (
      payload.relationReferences ?? payload.relation_references ?? []
    ).map((row) => normalizeKnowledgeBundleRelationReference(row)),
    evidenceReferences: (
      payload.evidenceReferences ?? payload.evidence_references ?? []
    ).map((row) => normalizeKnowledgeBundleEvidenceReference(row)),
  }
}
