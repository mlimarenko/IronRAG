import type { RawDocumentResponse } from '@/api/documents';
import type {
  RawGraphDocumentLink,
  RawKnowledgeDocument,
  RawKnowledgeEntity,
  RawKnowledgeEntityDetail,
  RawKnowledgeRelation,
} from '@/types/api-responses';
import type {
  GraphEdge,
  GraphMetadata,
  GraphNode,
  GraphNodeType,
  GraphStatus,
} from '@/types';

type GraphTopologyPayload = {
  entities?: unknown;
  relations?: unknown;
  documents?: unknown;
  documentLinks?: unknown;
  document_links?: unknown;
  status?: GraphStatus;
  convergenceStatus?: string;
};

export type GraphTopology = {
  nodes: GraphNode[];
  edges: GraphEdge[];
  meta: GraphMetadata;
};

function normalizeArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

export function mapNodeType(t: string | undefined): GraphNodeType {
  if (t === 'document') return 'document';
  if (t === 'person') return 'person';
  if (t === 'organization') return 'organization';
  if (t === 'location') return 'location';
  if (t === 'event') return 'event';
  if (t === 'artifact') return 'artifact';
  if (t === 'natural') return 'natural';
  if (t === 'process') return 'process';
  if (t === 'concept') return 'concept';
  if (t === 'attribute') return 'attribute';
  return 'entity';
}

function countConnectedComponents(nodes: GraphNode[], edges: GraphEdge[]): number {
  if (nodes.length === 0) return 0;

  const adjacency = new Map<string, string[]>();
  for (const node of nodes) {
    adjacency.set(node.id, []);
  }

  for (const edge of edges) {
    if (edge.sourceId === edge.targetId) continue;
    const sourceNeighbors = adjacency.get(edge.sourceId);
    const targetNeighbors = adjacency.get(edge.targetId);
    if (!sourceNeighbors || !targetNeighbors) continue;
    sourceNeighbors.push(edge.targetId);
    targetNeighbors.push(edge.sourceId);
  }

  let componentCount = 0;
  const visited = new Set<string>();

  // Head-pointer BFS. Using `queue.shift()` is O(n) per pop in V8, so a
  // BFS over 25k nodes/80k edges ran O(N²) ≈ hundreds of millions of ops
  // and blocked the main thread long enough to stall the graph page at
  // prod scale. Incrementing a head index is O(1) per pop.
  const queue: string[] = [];
  for (const node of nodes) {
    if (visited.has(node.id)) continue;
    componentCount += 1;

    queue.length = 0;
    queue.push(node.id);
    visited.add(node.id);
    let head = 0;

    while (head < queue.length) {
      const current = queue[head];
      head += 1;

      const neighbors = adjacency.get(current);
      if (!neighbors) continue;
      for (const neighbor of neighbors) {
        if (visited.has(neighbor)) continue;
        visited.add(neighbor);
        queue.push(neighbor);
      }
    }
  }

  return componentCount;
}

export type GraphLayoutHint = 'sectors' | 'bands' | 'components' | 'rings' | 'clusters';

export function recommendGraphLayout(
  nodes: GraphNode[],
  edges: GraphEdge[],
): GraphLayoutHint {
  if (nodes.length === 0) return 'bands';

  const typeCount = new Set(nodes.map((node) => node.type)).size;
  const componentCount = countConnectedComponents(nodes, edges);

  if (componentCount >= 6 && edges.length < nodes.length * 2.2) {
    return 'components';
  }

  if (nodes.length > 320 || edges.length > nodes.length * 2.8 || typeCount >= 6) {
    return 'bands';
  }

  return 'sectors';
}

export function mapGraphTopology(raw: unknown): GraphTopology {
  const topology: GraphTopologyPayload = (raw as GraphTopologyPayload) ?? {};
  const entities = normalizeArray<RawKnowledgeEntity>(topology.entities);
  const relations = normalizeArray<RawKnowledgeRelation>(topology.relations);
  const documents = normalizeArray<RawKnowledgeDocument>(topology.documents);
  const documentLinksRaw = normalizeArray<RawGraphDocumentLink>(topology.documentLinks);
  const documentLinks =
    documentLinksRaw.length > 0
      ? documentLinksRaw
      : normalizeArray<RawGraphDocumentLink>(topology.document_links);

  const docEdgeCounts = new Map<string, number>();
  documentLinks.forEach((link) => {
    docEdgeCounts.set(link.documentId, (docEdgeCounts.get(link.documentId) ?? 0) + 1);
  });

  const entityNodes: GraphNode[] = entities.map((e) => {
    const canonical = mapNodeType(e.entityType);
    const rawType = (e.entityType ?? '').toLowerCase();
    return {
      id: e.entityId ?? e.id ?? '',
      label: e.canonicalLabel ?? e.label ?? e.key ?? 'unknown',
      type: canonical,
      subType: e.entitySubType ?? (rawType !== canonical ? rawType : undefined),
      summary: e.summary ?? undefined,
      edgeCount: e.supportCount ?? 0,
      properties: {},
      sourceDocumentIds: [],
    };
  });

  const documentNodes: GraphNode[] = documents.map((d) => {
    const docId = d.document_id ?? d.documentId ?? d.id ?? '';
    return {
      id: docId,
      label: d.title ?? d.fileName ?? d.external_key ?? 'untitled',
      type: 'document' as GraphNodeType,
      summary: undefined,
      edgeCount: docEdgeCounts.get(docId) ?? 0,
      properties: {},
      sourceDocumentIds: [],
    };
  });

  const nodes: GraphNode[] = [...entityNodes, ...documentNodes];

  const relationEdges: GraphEdge[] = relations
    .map((r): GraphEdge | null => {
      if (!r.subjectEntityId || !r.objectEntityId) return null;
      return {
        id: r.relationId ?? r.id ?? '',
        sourceId: r.subjectEntityId,
        targetId: r.objectEntityId,
        label: r.predicate ?? '',
        weight: r.supportCount ?? 1,
      };
    })
    .filter((edge): edge is GraphEdge => edge !== null);

  const documentEdges: GraphEdge[] = documentLinks.map((link) => ({
    id: `dl-${link.documentId}-${link.targetNodeId}`,
    sourceId: link.documentId,
    targetId: link.targetNodeId,
    label: 'supports',
    weight: link.supportCount ?? 1,
  }));

  const edges: GraphEdge[] = [...relationEdges, ...documentEdges];

  const recommendedLayout = recommendGraphLayout(nodes, edges);
  const status = topology.status ?? (nodes.length > 0 ? 'ready' : 'empty');

  const meta: GraphMetadata = {
    nodeCount: nodes.length,
    edgeCount: edges.length,
    hiddenDisconnectedCount: 0,
    status,
    convergenceStatus: topology.convergenceStatus ?? 'current',
    recommendedLayout,
  };

  return { nodes, edges, meta };
}

/**
 * Map the entity-detail response onto the inspector's `GraphNode` view model.
 * Falls back to fields from `basic` (the node already in the list) so the
 * inspector stays populated even when the detail endpoint omits a field.
 * Returns `sourceDocumentIds` populated from `supportingDocuments` when the
 * detail exposes `selectedNode.relatedNodes`, matching the legacy behavior
 * in GraphPage.
 */
export function mapKnowledgeEntityDetail(
  raw: unknown,
  basic: GraphNode | null,
  selectedId: string,
): GraphNode {
  const detail = (raw as RawKnowledgeEntityDetail) ?? {};
  const entity: RawKnowledgeEntity =
    detail.entity ?? (detail as unknown as RawKnowledgeEntity);
  const canonicalType = mapNodeType(entity.entityType ?? entity.nodeType);
  const rawType = (entity.entityType ?? '').toLowerCase();
  const resolvedSubType =
    entity.entitySubType ??
    basic?.subType ??
    (rawType !== canonicalType ? rawType : undefined);

  const enriched: GraphNode = {
    id: entity.entityId ?? entity.id ?? selectedId,
    label: entity.canonicalLabel ?? entity.label ?? basic?.label ?? '',
    type: canonicalType,
    subType: resolvedSubType,
    summary: entity.summary ?? basic?.summary ?? undefined,
    edgeCount: entity.supportCount ?? basic?.edgeCount ?? 0,
    properties: {},
    sourceDocumentIds: [],
  };

  if (entity.entityType) enriched.properties['type'] = entity.entityType;
  if (entity.confidence != null) {
    enriched.properties['confidence'] =
      String(Math.round(entity.confidence * 100)) + '%';
  }
  if (entity.supportCount != null) {
    enriched.properties['support count'] = String(entity.supportCount);
  }
  if (entity.entityState) enriched.properties['state'] = entity.entityState;
  if (entity.aliases?.length) enriched.properties['aliases'] = entity.aliases.join(', ');

  if (detail.selectedNode?.relatedNodes) {
    enriched.sourceDocumentIds = (detail.selectedNode.supportingDocuments ?? []).map(
      (d) => d.documentId,
    );
  }

  return enriched;
}

type RawGraphDocumentRevision = {
  mime_type?: string;
  byte_size?: number;
  revision_number?: number;
  content_source_kind?: string;
  source_uri?: string;
};

function mapGraphDocumentSummary(raw: RawDocumentResponse): string | undefined {
  const head =
    typeof raw.head === 'object' && raw.head !== null
      ? (raw.head as Record<string, unknown>)
      : null;
  const summary = head?.documentSummary ?? head?.document_summary;
  return typeof summary === 'string' && summary.trim().length > 0 ? summary : undefined;
}

function mapGraphDocumentRevision(raw: RawDocumentResponse): RawGraphDocumentRevision | undefined {
  const revision = raw.activeRevision ?? raw.active_revision;
  if (!revision || typeof revision !== 'object') {
    return undefined;
  }
  return revision as RawGraphDocumentRevision;
}

export function mapGraphDocumentDetail(
  raw: RawDocumentResponse,
  basic: GraphNode | null,
  selectedId: string,
): GraphNode {
  const revision = mapGraphDocumentRevision(raw);
  const isWebPage = revision?.content_source_kind === 'web_page';
  const fileNameLabel = typeof raw.fileName === 'string' ? raw.fileName : undefined;
  const label =
    (isWebPage ? revision?.source_uri : undefined) ?? fileNameLabel ?? basic?.label ?? selectedId;

  const enriched: GraphNode = {
    id: selectedId,
    label,
    type: 'document',
    summary: mapGraphDocumentSummary(raw) ?? basic?.summary,
    edgeCount: basic?.edgeCount ?? 0,
    properties: {},
    sourceDocumentIds: [],
  };

  if (revision?.mime_type) {
    enriched.properties['format'] = revision.mime_type;
  }
  if (revision?.byte_size != null) {
    enriched.properties['size'] = `${(revision.byte_size / 1024).toFixed(1)} KB`;
  }
  if (revision?.revision_number != null) {
    enriched.properties['revision'] = String(revision.revision_number);
  }
  enriched.properties['state'] = raw.readinessSummary?.readinessKind ?? 'unknown';
  enriched.properties['activity'] = raw.readinessSummary?.activityStatus ?? 'unknown';
  if (raw.readinessSummary?.graphCoverageKind) {
    enriched.properties['graph coverage'] = raw.readinessSummary.graphCoverageKind;
  }

  return enriched;
}
