import { Knowledge } from './generated'
import type { KnowledgeEntityDetailResponse, KnowledgeLibrarySummaryResponse } from './generated'
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

/**
 * Streams the primary compact NDJSON graph topology and reconstructs it
 * into the graph UI topology model consumed by GraphPage and GraphCanvas.
 * The wire format uses dense numeric ids and short field
 * keys to shave ~50% off the uncompressed payload; this adapter walks
 * each NDJSON frame once, rebuilding real UUIDs via the id_map section.
 *
 * See `apps/api/src/services/knowledge/graph_stream.rs` for the full
 * wire-format description and field-key legend.
 */
async function getGraphTopologyStream(
  libraryId: string,
  options: { onProgress?: (progress: GraphTopologyProgress) => void } = {},
): Promise<KnowledgeGraphTopologyResponse> {
  const response = await fetchGraphTopology(libraryId)
  if (!response.body) {
    throw new Error('graph topology response body missing')
  }

  const topologyState = createTopologyState()
  const consumeFrame = createTopologyFrameConsumer(topologyState)
  const emitProgress = (): void => {
    options.onProgress?.(getTopologyProgress(topologyState))
  }

  await consumeTopologyResponse(response.body, consumeFrame, emitProgress)
  emitProgress()

  return getTopologyResponse(topologyState)
}

interface GraphTopologyProgress {
  nodesLoaded: number
  edgesLoaded: number
  expectedNodes: number
  expectedEdges: number
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

async function fetchGraphTopology(libraryId: string): Promise<Response> {
  const response = await fetch(`${API_BASE}/knowledge/libraries/${libraryId}/graph`, {
    credentials: 'include',
    headers: { Accept: 'application/x-ndjson' },
  })
  if (response.ok) return response

  reportUnauthorizedResponse(response.status)
  throw new KnowledgeApiError(response.status, await getGraphTopologyProblemDetails(response))
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

function createTopologyState(): TopologyState {
  return {
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
  const decoder = new TextDecoder()
  let buffer = new Uint8Array(256 * 1024)
  let bufferLength = 0

  const decodeFrame = (bytes: Uint8Array, context: string): void => {
    const text = decoder.decode(bytes).trim()
    if (!text) return
    try {
      applyTopologyFrame(state, JSON.parse(text) as TopologyFrame)
    } catch (error) {
      console.warn(`graph topology ${context} frame parse failed`, error)
    }
  }

  const consumeLines = (flush: boolean): void => {
    let start = 0
    for (let index = 0; index < bufferLength; index += 1) {
      if (buffer[index] !== 0x0a) continue
      if (index > start) decodeFrame(buffer.subarray(start, index), 'stream')
      start = index + 1
    }
    if (start > 0) {
      buffer.copyWithin(0, start, bufferLength)
      bufferLength -= start
    }
    if (flush && bufferLength > 0) {
      decodeFrame(buffer.subarray(0, bufferLength), 'tail')
      bufferLength = 0
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
  }
}

async function consumeTopologyResponse(
  body: ReadableStream<Uint8Array>,
  consumeFrame: (chunk: Uint8Array | null) => void,
  emitProgress: () => void,
): Promise<void> {
  const reader = body.getReader()
  while (true) {
    const { value, done } = await reader.read()
    if (done) break
    if (value && value.length > 0) consumeFrame(value)
    emitProgress()
  }
  consumeFrame(null)
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
  numToUuid: Map<number, string>
  documents: KnowledgeTopologyDocument[]
  entities: KnowledgeTopologyEntity[]
  relations: KnowledgeTopologyRelation[]
  documentLinks: KnowledgeTopologyDocumentLink[]
  expectedNodes: number
  expectedEdges: number
}

function applyDocumentRows(state: TopologyState, data: unknown): void {
  const rows = Array.isArray(data) ? (data as CompactDocRow[]) : []
  for (const row of rows) {
    if (typeof row.i !== 'number') continue
    const documentId = state.numToUuid.get(row.i)
    if (!documentId) continue
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
  const rows = Array.isArray(data) ? (data as CompactEntityRow[]) : []
  for (const row of rows) {
    if (typeof row.i !== 'number') continue
    const entityId = state.numToUuid.get(row.i)
    if (!entityId) continue
    state.entities.push(topologyEntityFromRow(row, entityId))
  }
}

function applyEdgeRows(state: TopologyState, data: unknown): void {
  const rows = Array.isArray(data) ? (data as CompactEdgeTuple[]) : []
  for (const tuple of rows) {
    if (!Array.isArray(tuple) || tuple.length < 4) continue
    const [fromNum, toNum, predicate, supportCount] = tuple
    const subjectEntityId = state.numToUuid.get(fromNum)
    const objectEntityId = state.numToUuid.get(toNum)
    if (!subjectEntityId || !objectEntityId) continue
    state.relations.push({ subjectEntityId, objectEntityId, predicate, supportCount })
  }
}

function applyDocumentLinkRows(state: TopologyState, data: unknown): void {
  const rows = Array.isArray(data) ? (data as CompactDocLinkTuple[]) : []
  for (const tuple of rows) {
    if (!Array.isArray(tuple) || tuple.length < 4) continue
    const [documentNumber, targetNumber, , supportCount] = tuple
    const documentId = state.numToUuid.get(documentNumber)
    const targetNodeId = state.numToUuid.get(targetNumber)
    if (!documentId || !targetNodeId) continue
    state.documentLinks.push({ documentId, targetNodeId, supportCount })
  }
}

function applyTopologyFrame(state: TopologyState, frame: TopologyFrame): void {
  switch (frame.s) {
    case 'meta':
      state.expectedNodes = Number(frame.node_count ?? 0)
      state.expectedEdges = Number(frame.edge_count ?? 0)
      return
    case 'id_map':
      for (const [uuid, number] of Object.entries(frame.m ?? {})) {
        if (typeof number === 'number') state.numToUuid.set(number, uuid)
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
      return
  }
}

export const knowledgeApi = {
  // The graph topology endpoint streams NDJSON; the generated SDK cannot model
  // that wire format, so getGraphTopologyStream stays the primary client.
  getGraphTopology: (
    libraryId: string,
    options?: { onProgress?: (progress: GraphTopologyProgress) => void },
  ) => getGraphTopologyStream(libraryId, options),
  getEntity: (libraryId: string, entityId: string) =>
    Knowledge.getKnowledgeEntity({ path: { libraryId, entityId } }).then(
      (result): KnowledgeEntityDetailResponse => unwrap(result),
    ),
  getLibrarySummary: (libraryId: string) =>
    Knowledge.getKnowledgeLibrarySummary({ path: { libraryId } }).then(
      (result): KnowledgeLibrarySummaryResponse => unwrap(result),
    ),
}
