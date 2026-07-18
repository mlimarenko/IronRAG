import { Knowledge } from './generated'
import type { KnowledgeEntityDetailResponse, KnowledgeLibrarySummaryResponse } from './generated'
import { unwrap } from './runtime'
import type {
  KnowledgeGraphTopologyResponse,
  KnowledgeTopologyDocument,
  KnowledgeTopologyDocumentLink,
  KnowledgeTopologyEntity,
  KnowledgeTopologyRelation,
} from '@/shared/types/graph-topology'

const API_BASE = '/v1'

/**
 * Streams the canonical compact NDJSON graph topology and reconstructs it
 * into the graph UI topology model consumed by GraphPage and SigmaGraph.
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
  const response = await fetch(`${API_BASE}/knowledge/libraries/${libraryId}/graph`, {
    credentials: 'include',
    headers: { Accept: 'application/x-ndjson' },
  })
  if (!response.ok) {
    let body: ApiErrorBody = {}
    try {
      body = (await response.json()) as ApiErrorBody
    } catch {
      /* fall through — non-json error response */
    }
    throw new KnowledgeApiError(response.status, body)
  }
  if (!response.body) {
    throw new Error('graph topology response body missing')
  }

  const reader = response.body.getReader()
  const decoder = new TextDecoder()

  // Byte-level buffer. The previous implementation held a growing
  // UTF-8 *string* (`let pending = "";`) and repeatedly did
  // `pending += chunk; pending = pending.slice(newlineIndex + 1)` —
  // on a 7.87 MB large-library reference payload that is O(N²) in engine heap churn
  // because each `+=` and each `slice` allocates a fresh String.
  // A Uint8Array ring keeps the live bytes contiguous, lets us scan
  // for 0x0A directly, and only decodes the slice that spans a full
  // NDJSON line. Measured ~150 ms saved on the large-library reference fixture.
  let buffer = new Uint8Array(256 * 1024)
  let bufferLength = 0
  const LF = 0x0a
  const appendBytes = (chunk: Uint8Array) => {
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
  }
  const consumeLines = (flush: boolean): void => {
    let start = 0
    for (let index = 0; index < bufferLength; index += 1) {
      if (buffer[index] !== LF) continue
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

  const topologyState: TopologyState = {
    numToUuid: new Map<number, string>(),
    documents: [],
    entities: [],
    relations: [],
    documentLinks: [],
    expectedNodes: 0,
    expectedEdges: 0,
  }
  const handleFrame = (frame: TopologyFrame): void => applyTopologyFrame(topologyState, frame)
  const decodeFrame = (bytes: Uint8Array, context: string): void => {
    const text = decoder.decode(bytes).trim()
    if (!text) return
    try {
      handleFrame(JSON.parse(text) as TopologyFrame)
    } catch (err) {
      console.warn(`graph topology ${context} frame parse failed`, err)
    }
  }

  const emitProgress = (): void => {
    options.onProgress?.({
      nodesLoaded: topologyState.entities.length + topologyState.documents.length,
      edgesLoaded: topologyState.relations.length + topologyState.documentLinks.length,
      expectedNodes: topologyState.expectedNodes,
      expectedEdges: topologyState.expectedEdges,
    })
  }

  // Consume frames as soon as bytes arrive.
  while (true) {
    const { value, done } = await reader.read()
    if (done) break
    if (value && value.length > 0) {
      appendBytes(value)
      consumeLines(false)
    }
    emitProgress()
  }
  consumeLines(true)
  emitProgress()

  return {
    documents: topologyState.documents,
    entities: topologyState.entities,
    relations: topologyState.relations,
    documentLinks: topologyState.documentLinks,
  }
}

interface GraphTopologyProgress {
  nodesLoaded: number
  edgesLoaded: number
  expectedNodes: number
  expectedEdges: number
}

interface ApiErrorBody {
  error?: string
  message?: string
  [key: string]: unknown
}

class KnowledgeApiError extends Error {
  constructor(
    public status: number,
    public body: ApiErrorBody,
  ) {
    super(body?.error || body?.message || `API error ${status}`)
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
      id: documentId,
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
    ['canonicalLabel', row.l],
    ['entityType', row.t],
    ['entitySubType', row.ts],
    ['confidence', row.c],
  ] as const
  const baseEntity: KnowledgeTopologyEntity = {
    id: entityId,
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
  // that wire format, so getGraphTopologyStream stays the canonical client.
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
