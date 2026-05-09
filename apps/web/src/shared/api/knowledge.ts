import { Knowledge } from "./generated";
import type {
  KnowledgeEntityDetailResponse,
  KnowledgeLibrarySummaryResponse,
} from "./generated";
import { unwrap } from "./runtime";
import type {
  KnowledgeGraphTopologyResponse,
  KnowledgeTopologyDocument,
  KnowledgeTopologyDocumentLink,
  KnowledgeTopologyEntity,
  KnowledgeTopologyRelation,
} from "@/shared/types/graph-topology";

const API_BASE = "/v1";

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
    credentials: "include",
    headers: { Accept: "application/x-ndjson" },
  });
  if (!response.ok) {
    let body: ApiErrorBody = {};
    try {
      body = (await response.json()) as ApiErrorBody;
    } catch {
      /* fall through — non-json error response */
    }
    throw new KnowledgeApiError(response.status, body);
  }
  if (!response.body) {
    throw new Error("graph topology response body missing");
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();

  // Byte-level buffer. The previous implementation held a growing
  // UTF-8 *string* (`let pending = "";`) and repeatedly did
  // `pending += chunk; pending = pending.slice(newlineIndex + 1)` —
  // on a 7.87 MB large-library reference payload that is O(N²) in engine heap churn
  // because each `+=` and each `slice` allocates a fresh String.
  // A Uint8Array ring keeps the live bytes contiguous, lets us scan
  // for 0x0A directly, and only decodes the slice that spans a full
  // NDJSON line. Measured ~150 ms saved on the large-library reference fixture.
  let buffer = new Uint8Array(256 * 1024);
  let bufferLength = 0;
  const LF = 0x0a;
  const appendBytes = (chunk: Uint8Array) => {
    const required = bufferLength + chunk.length;
    if (required > buffer.length) {
      let next = buffer.length;
      while (next < required) next *= 2;
      const grown = new Uint8Array(next);
      grown.set(buffer.subarray(0, bufferLength), 0);
      buffer = grown;
    }
    buffer.set(chunk, bufferLength);
    bufferLength = required;
  };
  const consumeLines = (flush: boolean): void => {
    let start = 0;
    for (let i = 0; i < bufferLength; i += 1) {
      if (buffer[i] !== LF) continue;
      // [start, i) is one complete NDJSON line.
      if (i > start) {
        const line = decoder.decode(buffer.subarray(start, i));
        const trimmed = line.trim();
        if (trimmed.length > 0) {
          try {
            handleFrame(JSON.parse(trimmed) as TopologyFrame);
          } catch (err) {
            console.warn("graph topology frame parse failed", err);
          }
        }
      }
      start = i + 1;
    }
    if (start > 0) {
      buffer.copyWithin(0, start, bufferLength);
      bufferLength -= start;
    }
    if (flush && bufferLength > 0) {
      const tail = decoder.decode(buffer.subarray(0, bufferLength)).trim();
      if (tail.length > 0) {
        try {
          handleFrame(JSON.parse(tail) as TopologyFrame);
        } catch (err) {
          console.warn("graph topology tail frame parse failed", err);
        }
      }
      bufferLength = 0;
    }
  };

  const numToUuid = new Map<number, string>();
  const documents: KnowledgeTopologyDocument[] = [];
  const entities: KnowledgeTopologyEntity[] = [];
  const relations: KnowledgeTopologyRelation[] = [];
  const documentLinks: KnowledgeTopologyDocumentLink[] = [];
  let expectedNodes = 0;
  let expectedEdges = 0;

  const handleFrame = (frame: TopologyFrame): void => {
    switch (frame.s) {
      case "meta": {
        expectedNodes = Number(frame.node_count ?? 0);
        expectedEdges = Number(frame.edge_count ?? 0);
        break;
      }
      case "id_map": {
        const raw = frame.m ?? {};
        for (const [uuid, num] of Object.entries(raw)) {
          if (typeof num === "number") {
            numToUuid.set(num, uuid);
          }
        }
        break;
      }
      case "docs": {
        const rows = Array.isArray(frame.d) ? frame.d : [];
        for (const row of rows as CompactDocRow[]) {
          if (typeof row.i !== "number") continue;
          const documentId = numToUuid.get(row.i);
          if (!documentId) continue;
          const document: KnowledgeTopologyDocument = {
            id: documentId,
            documentId,
          };
          if (row.t !== undefined) document.title = row.t;
          if (row.fn !== undefined) document.fileName = row.fn;
          if (row.k !== undefined) document.external_key = row.k;
          documents.push(document);
        }
        break;
      }
      case "nodes": {
        const rows = Array.isArray(frame.d) ? frame.d : [];
        for (const row of rows as CompactEntityRow[]) {
          if (typeof row.i !== "number") continue;
          const entityId = numToUuid.get(row.i);
          if (!entityId) continue;
          const entity: KnowledgeTopologyEntity = {
            id: entityId,
            entityId,
            summary: row.sm ?? null,
            supportCount: row.s ?? 1,
            entityState: row.es ?? "active",
            aliases: row.a ?? [],
          };
          if (row.k !== undefined) entity.key = row.k;
          if (row.l !== undefined) entity.canonicalLabel = row.l;
          if (row.t !== undefined) entity.entityType = row.t;
          if (row.ts !== undefined) entity.entitySubType = row.ts;
          if (row.c !== undefined) entity.confidence = row.c;
          entities.push(entity);
        }
        break;
      }
      case "edges": {
        const rows = Array.isArray(frame.d) ? frame.d : [];
        for (const tuple of rows as CompactEdgeTuple[]) {
          if (!Array.isArray(tuple) || tuple.length < 4) continue;
          const [fromNum, toNum, relationType, supportCount] = tuple;
          const subject = numToUuid.get(fromNum);
          const object = numToUuid.get(toNum);
          if (!subject || !object) continue;
          relations.push({
            subjectEntityId: subject,
            objectEntityId: object,
            predicate: relationType,
            supportCount,
          });
        }
        break;
      }
      case "doc_links": {
        const rows = Array.isArray(frame.d) ? frame.d : [];
        for (const tuple of rows as CompactDocLinkTuple[]) {
          if (!Array.isArray(tuple) || tuple.length < 4) continue;
          const [docNum, targetNum, , supportCount] = tuple;
          const documentId = numToUuid.get(docNum);
          const targetNodeId = numToUuid.get(targetNum);
          if (!documentId || !targetNodeId) continue;
          documentLinks.push({ documentId, targetNodeId, supportCount });
        }
        break;
      }
      case "end":
      default:
        break;
    }
  };

  const emitProgress = (): void => {
    options.onProgress?.({
      nodesLoaded: entities.length + documents.length,
      edgesLoaded: relations.length + documentLinks.length,
      expectedNodes,
      expectedEdges,
    });
  };

  // Consume frames as soon as bytes arrive.
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    if (value && value.length > 0) {
      appendBytes(value);
      consumeLines(false);
    }
    emitProgress();
  }
  consumeLines(true);
  emitProgress();

  return {
    documents,
    entities,
    relations,
    documentLinks,
  };
}

interface GraphTopologyProgress {
  nodesLoaded: number;
  edgesLoaded: number;
  expectedNodes: number;
  expectedEdges: number;
}

interface ApiErrorBody {
  error?: string;
  message?: string;
  [key: string]: unknown;
}

class KnowledgeApiError extends Error {
  constructor(public status: number, public body: ApiErrorBody) {
    super(body?.error || body?.message || `API error ${status}`);
  }
}

type TopologyFrame =
  | { s: "meta"; node_count?: number; edge_count?: number; [key: string]: unknown }
  | { s: "id_map"; m: Record<string, unknown> }
  | { s: "docs"; d: unknown }
  | { s: "nodes"; d: unknown }
  | { s: "edges"; d: unknown }
  | { s: "doc_links"; d: unknown }
  | { s: "end"; [key: string]: unknown };

interface CompactDocRow {
  i: number;
  k?: string;
  t?: string;
  fn?: string;
}

interface CompactEntityRow {
  i: number;
  l?: string;
  k?: string;
  t?: string;
  ts?: string;
  s?: number;
  c?: number;
  es?: string;
  a?: string[];
  sm?: string;
}

type CompactEdgeTuple = [number, number, string, number];
type CompactDocLinkTuple = [number, number, string, number];

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
};
