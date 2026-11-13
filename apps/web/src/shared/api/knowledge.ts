import { Knowledge } from './generated'
import type {
  KnowledgeEntityDetailResponse,
  KnowledgeGraphPreparationResponse,
  KnowledgeLibrarySummaryResponse,
} from './generated'
import { unwrap, type ApiErrorBody } from './runtime'
import { reportUnauthorizedResponse } from './sessionLifecycle'
import type {
  KnowledgeGraphTopologyResponse,
  KnowledgeTopologyDocument,
  KnowledgeTopologyDocumentLink,
  KnowledgeTopologyEntity,
  KnowledgeTopologyRelation,
} from '@/shared/types/graph-topology'

const API_BASE = '/v1'
const MIN_PREPARATION_POLL_MS = 1000

/**
 * Prepares the complete topology, then validates its streamed NDJSON before
 * returning the graph model consumed by GraphPage and GraphCanvas.
 * Dense numeric references are resolved through the batched id_map section.
 *
 * The wire format is rendered by
 * `apps/api/src/services/knowledge/graph_stream/render.rs`.
 */
async function getGraphTopologyStream(
  libraryId: string,
  options: GraphTopologyOptions = {},
): Promise<KnowledgeGraphTopologyResponse> {
  options.signal?.throwIfAborted()
  const preparation = await prepareGraphTopology(libraryId, options.signal)
  const response = await fetchGraphTopology(
    `${graphPreparationUrl(libraryId)}/${encodeURIComponent(preparation.id)}/result`,
    options.signal,
    'application/x-ndjson',
  )
  options.signal?.throwIfAborted()
  if (!response.body) {
    throw new Error('graph topology response body missing')
  }

  const topologyState = createTopologyState(preparation)
  const consumeFrame = createTopologyFrameConsumer(topologyState)
  const emitProgress = (): void => {
    options.onProgress?.(getTopologyProgress(topologyState))
  }

  await consumeTopologyResponse(response.body, consumeFrame, emitProgress, options.signal)
  options.signal?.throwIfAborted()
  emitProgress()

  return getTopologyResponse(topologyState)
}

interface GraphTopologyProgress {
  nodesLoaded: number
  edgesLoaded: number
  expectedNodes: number
  expectedEdges: number
}

type GraphTopologyOptions = {
  signal?: AbortSignal
  onProgress?: (progress: GraphTopologyProgress) => void
}

type GraphTopologyProblemDetails = ApiErrorBody

class KnowledgeApiError extends Error {
  constructor(
    public status: number,
    public body: GraphTopologyProblemDetails,
  ) {
    super(body.detail || body.title || `API error ${status}`)
  }
}

export class GraphPreparationError extends Error {
  constructor(public status: 'failed' | 'expired' | 'superseded') {
    super(`Graph preparation ${status}`)
  }
}

type GraphPreparation = KnowledgeGraphPreparationResponse

function graphPreparationUrl(libraryId: string): string {
  return `${API_BASE}/knowledge/libraries/${encodeURIComponent(libraryId)}/graph/preparations`
}

async function fetchGraphTopology(
  url: string,
  signal?: AbortSignal,
  accept = 'application/json',
  method = 'GET',
): Promise<Response> {
  const response = await fetch(url, {
    credentials: 'include',
    method,
    headers: { Accept: accept },
    ...(signal ? { signal } : {}),
  })
  if (response.ok) return response

  reportUnauthorizedResponse(response.status)
  const details = await getGraphTopologyProblemDetails(response)
  signal?.throwIfAborted()
  throw new KnowledgeApiError(response.status, details)
}

async function prepareGraphTopology(
  libraryId: string,
  signal?: AbortSignal,
): Promise<GraphPreparation> {
  const base = graphPreparationUrl(libraryId)
  let response = await fetchGraphTopology(base, signal, 'application/json', 'POST')
  let initial: GraphPreparation | undefined
  while (true) {
    const value: unknown = await response.json()
    if (!value || typeof value !== 'object') throw new Error('Invalid graph preparation')
    const p = value as GraphPreparation
    if (
      typeof p.id !== 'string' ||
      !p.id ||
      p.libraryId !== libraryId ||
      p.formatVersion !== 1 ||
      (p.expiresAt !== null && typeof p.expiresAt !== 'string') ||
      (p.errorCode !== null && typeof p.errorCode !== 'string') ||
      ![
        p.projectionVersion,
        p.topologyGeneration,
        p.sourceTruthVersion,
        p.attempts,
        p.retryAfterMs,
      ].every((n) => Number.isSafeInteger(n) && n >= 0) ||
      (initial &&
        (p.id !== initial.id ||
          p.formatVersion !== initial.formatVersion ||
          p.projectionVersion !== initial.projectionVersion ||
          p.topologyGeneration !== initial.topologyGeneration ||
          p.sourceTruthVersion !== initial.sourceTruthVersion))
    )
      throw new Error('Invalid graph preparation receipt')
    initial ??= p
    signal?.throwIfAborted()
    if (p.status === 'ready') return p
    if (p.status === 'failed' || p.status === 'expired' || p.status === 'superseded')
      throw new GraphPreparationError(p.status)
    if (!['queued', 'validating', 'building', 'publishing', 'retrying'].includes(p.status))
      throw new Error('Invalid graph preparation status')
    await new Promise<void>((resolve, reject) => {
      const abort = () => {
        clearTimeout(timer)
        reject(
          signal?.reason instanceof Error
            ? signal.reason
            : new DOMException('Aborted', 'AbortError'),
        )
      }
      const timer = setTimeout(
        () => {
          signal?.removeEventListener('abort', abort)
          resolve()
        },
        Math.max(MIN_PREPARATION_POLL_MS, Math.min(p.retryAfterMs, 30000)),
      )
      signal?.addEventListener('abort', abort, { once: true })
      if (signal?.aborted) abort()
    })
    response = await fetchGraphTopology(`${base}/${encodeURIComponent(p.id)}`, signal)
  }
}

async function getGraphTopologyProblemDetails(
  response: Response,
): Promise<GraphTopologyProblemDetails> {
  const body: GraphTopologyProblemDetails = { status: response.status }
  try {
    const parsed = (await response.json()) as Record<string, unknown>
    return {
      ...(typeof parsed.detail === 'string' ? { detail: parsed.detail } : {}),
      ...(typeof parsed.code === 'string' ? { code: parsed.code } : {}),
      ...(typeof parsed.title === 'string' ? { title: parsed.title } : {}),
      status: response.status,
    }
  } catch {
    return body
  }
}

function createTopologyState(receipt: GraphPreparation): TopologyState {
  return {
    receipt,
    section: -1,
    ended: false,
    expectedDocuments: 0,
    documentIds: new Set(),
    entityIds: new Set(),
    uuids: new Set(),
    numToUuid: new Map<number, string>(),
    documents: [],
    entities: [],
    relations: [],
    documentLinks: [],
    expectedNodes: 0,
    expectedEdges: 0,
  }
}

function getTopologyProgress(state: TopologyState): GraphTopologyProgress {
  return {
    nodesLoaded: state.entities.length + state.documents.length,
    edgesLoaded: state.relations.length + state.documentLinks.length,
    expectedNodes: state.expectedNodes,
    expectedEdges: state.expectedEdges,
  }
}

function getTopologyResponse(state: TopologyState): KnowledgeGraphTopologyResponse {
  return {
    documents: state.documents,
    entities: state.entities,
    relations: state.relations,
    documentLinks: state.documentLinks,
  }
}

// Keep undecoded bytes in a contiguous Uint8Array so chunk assembly does not
// repeatedly allocate growing UTF-8 strings for large graph payloads.
function createTopologyFrameConsumer(state: TopologyState): (chunk: Uint8Array | null) => void {
  const decoder = new TextDecoder('utf-8', { fatal: true })
  let buffer = new Uint8Array(256 * 1024)
  let bufferLength = 0

  const decodeFrame = (bytes: Uint8Array): void => {
    const text = decoder.decode(bytes).trim()
    if (!text) throw new Error('Empty graph frame')
    applyTopologyFrame(state, JSON.parse(text) as TopologyFrame)
  }

  const consumeLines = (flush: boolean): void => {
    let start = 0
    for (let index = 0; index < bufferLength; index += 1) {
      if (buffer[index] !== 0x0a) continue
      if (index === start) throw new Error('Empty graph frame')
      decodeFrame(buffer.subarray(start, index))
      start = index + 1
    }
    if (start > 0) {
      buffer.copyWithin(0, start, bufferLength)
      bufferLength -= start
    }
    if (flush && bufferLength > 0) {
      throw new Error('Truncated graph topology frame')
    }
  }

  return (chunk: Uint8Array | null): void => {
    if (chunk) {
      const required = bufferLength + chunk.length
      if (required > buffer.length) {
        let next = buffer.length
        while (next < required) next *= 2
        const grown = new Uint8Array(next)
        grown.set(buffer.subarray(0, bufferLength), 0)
        buffer = grown
      }
      buffer.set(chunk, bufferLength)
      bufferLength = required
      consumeLines(false)
      return
    }
    consumeLines(true)
    if (!state.ended) throw new Error('Graph topology terminal frame missing')
  }
}

async function consumeTopologyResponse(
  body: ReadableStream<Uint8Array>,
  consumeFrame: (chunk: Uint8Array | null) => void,
  emitProgress: () => void,
  signal?: AbortSignal,
): Promise<void> {
  const reader = body.getReader()
  const abort = () => {
    // The read promise carries stream failures; cancellation can reject after a fetch abort.
    void reader.cancel(signal?.reason).catch(() => undefined)
  }
  signal?.addEventListener('abort', abort, { once: true })
  try {
    signal?.throwIfAborted()
    while (true) {
      const { value, done } = await reader.read()
      signal?.throwIfAborted()
      if (done) break
      if (value && value.length > 0) consumeFrame(value)
      emitProgress()
    }
    consumeFrame(null)
  } catch (error) {
    await reader.cancel(error).catch(() => undefined)
    throw error
  } finally {
    signal?.removeEventListener('abort', abort)
    reader.releaseLock()
  }
}

type TopologyFrame =
  | { s: 'meta'; node_count?: number; edge_count?: number; [key: string]: unknown }
  | { s: 'id_map'; m: Record<string, unknown> }
  | { s: 'docs'; d: unknown }
  | { s: 'nodes'; d: unknown }
  | { s: 'edges'; d: unknown }
  | { s: 'doc_links'; d: unknown }
  | { s: 'end'; [key: string]: unknown }

interface CompactDocRow {
  i: number
  k?: string
  t?: string
  fn?: string
}

interface CompactEntityRow {
  i: number
  l?: string
  k?: string
  t?: string
  ts?: string
  s?: number
  c?: number
  es?: string
  a?: string[]
  sm?: string
}

type CompactEdgeTuple = [number, number, string, number]
type CompactDocLinkTuple = [number, number, string, number]

type TopologyState = {
  receipt: GraphPreparation
  section: number
  ended: boolean
  expectedDocuments: number
  documentIds: Set<number>
  entityIds: Set<number>
  uuids: Set<string>
  numToUuid: Map<number, string>
  documents: KnowledgeTopologyDocument[]
  entities: KnowledgeTopologyEntity[]
  relations: KnowledgeTopologyRelation[]
  documentLinks: KnowledgeTopologyDocumentLink[]
  expectedNodes: number
  expectedEdges: number
}

function applyDocumentRows(state: TopologyState, data: unknown): void {
  if (!Array.isArray(data)) throw new Error('Invalid graph documents')
  const rows = data as CompactDocRow[]
  for (const row of rows) {
    if (
      !row ||
      !Number.isSafeInteger(row.i) ||
      state.documentIds.has(row.i) ||
      state.entityIds.has(row.i) ||
      [row.t, row.fn, row.k].some((v) => v !== undefined && typeof v !== 'string')
    )
      throw new Error('Invalid graph document')
    const documentId = state.numToUuid.get(row.i)
    if (!documentId) throw new Error('Unknown graph document ID')
    state.documentIds.add(row.i)
    state.documents.push({
      documentId,
      ...(row.t !== undefined ? { title: row.t } : {}),
      ...(row.fn !== undefined ? { fileName: row.fn } : {}),
      ...(row.k !== undefined ? { external_key: row.k } : {}),
    })
  }
}

function topologyEntityFromRow(row: CompactEntityRow, entityId: string): KnowledgeTopologyEntity {
  const optionalFields = [
    ['key', row.k],
    ['\u0063anonicalLabel', row.l],
    ['entityType', row.t],
    ['entitySubType', row.ts],
    ['confidence', row.c],
  ] as const
  const baseEntity: KnowledgeTopologyEntity = {
    entityId,
    summary: row.sm ?? null,
    supportCount: row.s ?? 1,
    entityState: row.es ?? 'active',
    aliases: row.a ?? [],
  }
  return optionalFields.reduce<KnowledgeTopologyEntity>(
    (entity, [key, value]) => (value === undefined ? entity : { ...entity, [key]: value }),
    baseEntity,
  )
}

function applyEntityRows(state: TopologyState, data: unknown): void {
  if (!Array.isArray(data)) throw new Error('Invalid graph nodes')
  const rows = data as CompactEntityRow[]
  for (const row of rows) {
    if (
      !row ||
      !Number.isSafeInteger(row.i) ||
      state.documentIds.has(row.i) ||
      state.entityIds.has(row.i) ||
      [row.l, row.k, row.t, row.ts, row.es, row.sm].some(
        (v) => v !== undefined && typeof v !== 'string',
      ) ||
      [row.s, row.c].some(
        (v) => v !== undefined && (typeof v !== 'number' || !Number.isFinite(v)),
      ) ||
      (row.a !== undefined && (!Array.isArray(row.a) || row.a.some((v) => typeof v !== 'string')))
    )
      throw new Error('Invalid graph node')
    const entityId = state.numToUuid.get(row.i)
    if (!entityId) throw new Error('Unknown graph node ID')
    state.entityIds.add(row.i)
    state.entities.push(topologyEntityFromRow(row, entityId))
  }
}

function applyEdgeRows(state: TopologyState, data: unknown): void {
  if (!Array.isArray(data)) throw new Error('Invalid graph edges')
  const rows = data as CompactEdgeTuple[]
  for (const tuple of rows) {
    if (!Array.isArray(tuple) || tuple.length !== 4) throw new Error('Invalid graph edge')
    const [fromNum, toNum, predicate, supportCount] = tuple
    const subjectEntityId = state.numToUuid.get(fromNum)
    const objectEntityId = state.numToUuid.get(toNum)
    if (
      !subjectEntityId ||
      !objectEntityId ||
      !state.entityIds.has(fromNum) ||
      !state.entityIds.has(toNum) ||
      typeof predicate !== 'string' ||
      typeof supportCount !== 'number' ||
      !Number.isFinite(supportCount)
    )
      throw new Error('Invalid graph edge reference')
    state.relations.push({ subjectEntityId, objectEntityId, predicate, supportCount })
  }
}

function applyDocumentLinkRows(state: TopologyState, data: unknown): void {
  if (!Array.isArray(data)) throw new Error('Invalid graph document links')
  const rows = data as CompactDocLinkTuple[]
  for (const tuple of rows) {
    if (!Array.isArray(tuple) || tuple.length !== 4) throw new Error('Invalid graph document link')
    const [documentNumber, targetNumber, predicate, supportCount] = tuple
    const documentId = state.numToUuid.get(documentNumber)
    const targetNodeId = state.numToUuid.get(targetNumber)
    if (
      !documentId ||
      !targetNodeId ||
      !state.documentIds.has(documentNumber) ||
      (!state.documentIds.has(targetNumber) && !state.entityIds.has(targetNumber)) ||
      typeof predicate !== 'string' ||
      typeof supportCount !== 'number' ||
      !Number.isFinite(supportCount)
    )
      throw new Error('Invalid graph document link reference')
    state.documentLinks.push({ documentId, targetNodeId, supportCount })
  }
}

function applyTopologyFrame(state: TopologyState, frame: TopologyFrame): void {
  if (!frame || typeof frame !== 'object' || state.ended) throw new Error('Invalid graph frame')
  const sections = ['meta', 'id_map', 'docs', 'nodes', 'edges', 'doc_links', 'end']
  const section = sections.indexOf(frame.s)
  if (
    section < 0 ||
    section < state.section ||
    (section === state.section && (section === 0 || section === 6)) ||
    (state.section === -1 && section !== 0) ||
    (section > 1 && state.section < 1)
  )
    throw new Error('Invalid graph frame order')
  state.section = section
  switch (frame.s) {
    case 'meta':
      if (
        frame.v !== state.receipt.formatVersion ||
        frame.library_id !== state.receipt.libraryId ||
        frame.preparation_id !== state.receipt.id ||
        frame.projection_version !== state.receipt.projectionVersion ||
        frame.topology_generation !== state.receipt.topologyGeneration ||
        frame.source_truth_version !== state.receipt.sourceTruthVersion ||
        typeof frame.generated_at !== 'string' ||
        ![frame.node_count, frame.edge_count, frame.document_count].every(
          (n) => typeof n === 'number' && Number.isSafeInteger(n) && n >= 0,
        )
      )
        throw new Error('Invalid graph metadata')
      state.expectedNodes = frame.node_count as number
      state.expectedEdges = frame.edge_count as number
      state.expectedDocuments = frame.document_count as number
      return
    case 'id_map':
      if (!frame.m || typeof frame.m !== 'object' || Array.isArray(frame.m))
        throw new Error('Invalid graph ID map')
      for (const [uuid, number] of Object.entries(frame.m)) {
        if (
          !uuid ||
          typeof number !== 'number' ||
          !Number.isSafeInteger(number) ||
          number < 0 ||
          state.numToUuid.has(number) ||
          state.uuids.has(uuid)
        )
          throw new Error('Invalid graph ID mapping')
        state.numToUuid.set(number, uuid)
        state.uuids.add(uuid)
      }
      return
    case 'docs':
      applyDocumentRows(state, frame.d)
      return
    case 'nodes':
      applyEntityRows(state, frame.d)
      return
    case 'edges':
      applyEdgeRows(state, frame.d)
      return
    case 'doc_links':
      applyDocumentLinkRows(state, frame.d)
      return
    case 'end':
      if (
        frame.preparation_id !== state.receipt.id ||
        frame.node_count !== state.expectedNodes ||
        frame.edge_count !== state.expectedEdges ||
        frame.document_count !== state.expectedDocuments ||
        state.documents.length !== state.expectedDocuments ||
        state.documents.length + state.entities.length !== state.expectedNodes ||
        state.relations.length + state.documentLinks.length !== state.expectedEdges
      )
        throw new Error('Invalid graph terminal counts')
      state.ended = true
      return
  }
}

export const knowledgeApi = {
  // The graph topology endpoint streams NDJSON; the generated SDK cannot model
  // that wire format, so getGraphTopologyStream stays the primary client.
  getGraphTopology: (libraryId: string, options?: GraphTopologyOptions) =>
    getGraphTopologyStream(libraryId, options),
  getEntity: (libraryId: string, entityId: string) =>
    Knowledge.getKnowledgeEntity({ path: { libraryId, entityId } }).then(
      (result): KnowledgeEntityDetailResponse => unwrap(result),
    ),
  getLibrarySummary: (libraryId: string) =>
    Knowledge.getKnowledgeLibrarySummary({ path: { libraryId } }).then(
      (result): KnowledgeLibrarySummaryResponse => unwrap(result),
    ),
}
