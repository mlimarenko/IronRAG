import { useState, useEffect, useRef, useMemo, lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { useNavigate } from 'react-router-dom';
import { documentsApi, knowledgeApi } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { compactText } from '@/lib/compactText';
import {
  GRAPH_LAYOUT_OPTIONS,
  GRAPH_NODE_COLORS,
  isGraphLayoutType,
  type GraphLayoutType,
} from '@/components/graph/config';
import {
  Search, X, Loader2,
  FileText, Share2, AlertTriangle,
  Eye, EyeOff, RotateCcw, Layers,
  PieChart, Rows3, Network, CircleDashed, Orbit,
} from 'lucide-react';
import type { GraphNode, GraphNodeType, GraphMetadata, GraphStatus } from '@/types';
import type {
  RawKnowledgeEntity,
  RawKnowledgeRelation,
  RawKnowledgeDocument,
  RawKnowledgeEntityDetail,
  RawGraphDocumentLink,
} from '@/types/api-responses';

function errorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message;
  if (typeof err === 'object' && err !== null && 'message' in err) {
    const msg = (err as { message?: unknown }).message;
    if (typeof msg === 'string') return msg;
  }
  return fallback;
}

function resolveDocumentSummary(raw: Record<string, unknown>): string | undefined {
  const head = typeof raw.head === 'object' && raw.head !== null
    ? (raw.head as Record<string, unknown>)
    : null;
  const summary = head?.documentSummary ?? head?.document_summary;
  return typeof summary === 'string' && summary.trim().length > 0 ? summary : undefined;
}


const SigmaGraph = lazy(() => import('@/components/SigmaGraph'));
const SUBTYPE_PREVIEW_LIMIT = 12;
const NO_SUBTYPE_KEY = '__no_subtype__';

const GRAPH_LAYOUT_ICONS = {
  sectors: PieChart,
  bands: Rows3,
  components: Network,
  rings: CircleDashed,
  clusters: Orbit,
} as const;

function mapNodeType(t: string | undefined): GraphNodeType {
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
  if (t === 'entity') return 'entity';
  // Backward compat for legacy type names
  if (t === 'topic') return 'concept';
  if (t === 'technology') return 'artifact';
  if (t === 'api') return 'artifact';
  if (t === 'code_symbol') return 'artifact';
  if (t === 'natural_kind') return 'natural';
  if (t === 'metric') return 'attribute';
  if (t === 'regulation') return 'artifact';
  return 'entity';
}

function subtypeFilterKey(type: string, subType?: string | null): string {
  return `${type}:${subType && subType.trim().length > 0 ? subType : NO_SUBTYPE_KEY}`;
}

function subtypeLegendLabel(t: (key: string, options?: Record<string, unknown>) => string, subType: string): string {
  return subType === NO_SUBTYPE_KEY ? t('graph.noSubType') : subType;
}

type GraphEdgeData = { id: string; sourceId: string; targetId: string; label: string; weight: number };

type RawGraphTopologyPayload = {
  entities?: unknown;
  relations?: unknown;
  documents?: unknown;
  documentLinks?: unknown;
  document_links?: unknown;
  status?: GraphStatus;
  convergenceStatus?: string;
};

function normalizeArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? value as T[] : [];
}

function readTopologyEntities(topology: RawGraphTopologyPayload): RawKnowledgeEntity[] {
  return normalizeArray<RawKnowledgeEntity>(topology.entities);
}

function readTopologyRelations(topology: RawGraphTopologyPayload): RawKnowledgeRelation[] {
  return normalizeArray<RawKnowledgeRelation>(topology.relations);
}

function readTopologyDocuments(topology: RawGraphTopologyPayload): RawKnowledgeDocument[] {
  return normalizeArray<RawKnowledgeDocument>(topology.documents);
}

function readTopologyDocumentLinks(topology: RawGraphTopologyPayload): RawGraphDocumentLink[] {
  return (
    normalizeArray<RawGraphDocumentLink>(topology.documentLinks).length > 0
      ? normalizeArray<RawGraphDocumentLink>(topology.documentLinks)
      : normalizeArray<RawGraphDocumentLink>(topology.document_links)
  );
}


function countConnectedComponents(nodes: GraphNode[], edges: GraphEdgeData[]): number {
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

  for (const node of nodes) {
    if (visited.has(node.id)) continue;
    componentCount += 1;

    const queue = [node.id];
    visited.add(node.id);

    while (queue.length > 0) {
      const current = queue.shift();
      if (!current) continue;

      for (const neighbor of adjacency.get(current) ?? []) {
        if (visited.has(neighbor)) continue;
        visited.add(neighbor);
        queue.push(neighbor);
      }
    }
  }

  return componentCount;
}

function recommendGraphLayout(nodes: GraphNode[], edges: GraphEdgeData[]): GraphLayoutType {
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


export default function GraphPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  // Edges from entities/relations fallback
  const edgesRef = useRef<GraphEdgeData[]>([]);

  // API state
  const [allNodes, setAllNodes] = useState<GraphNode[]>([]);
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null);
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building');
  const [loadError, setLoadError] = useState<string | null>(null);

  // Node detail state (from entity detail endpoint)
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [selectedDetail, setSelectedDetail] = useState<GraphNode | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [summaryExpanded, setSummaryExpanded] = useState(false);

  // UI controls
  const [searchQuery, setSearchQuery] = useState('');
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  const [hiddenSubTypes, setHiddenSubTypes] = useState<Set<string>>(new Set());
  const [layout, setLayout] = useState<GraphLayoutType>('bands');
  const [legendOpen, setLegendOpen] = useState(true);
  const [expandedSubtypeGroups, setExpandedSubtypeGroups] = useState<Set<string>>(new Set());
  const hasActiveGraphFilters = searchQuery.trim().length > 0 || hiddenTypes.size > 0 || hiddenSubTypes.size > 0;
  const hasActiveGraphState = hasActiveGraphFilters || selectedNode !== null;

  const resetGraphView = () => {
    setSelectedNode(null);
    setSelectedDetail(null);
    setSearchQuery('');
    setHiddenTypes(new Set());
    setHiddenSubTypes(new Set());
    setExpandedSubtypeGroups(new Set());
  };

  // Fetch graph data from the canonical topology endpoint only.
  useEffect(() => {
    if (!activeLibrary) return;
    let cancelled = false;
    setGraphStatus('building');
    setLoadError(null);
    setAllNodes([]);
    setGraphMeta(null);
    setSelectedNode(null);
    setSelectedDetail(null);
    setSearchQuery('');
    setHiddenTypes(new Set());
    setHiddenSubTypes(new Set());

    (async () => {
      try {
        const topologyRes = await knowledgeApi.getGraphTopology(activeLibrary.id);
        if (cancelled) return;

        const topology: RawGraphTopologyPayload = (topologyRes as RawGraphTopologyPayload) ?? {};
        const entities = readTopologyEntities(topology);
        const relations = readTopologyRelations(topology);
        const documents = readTopologyDocuments(topology);
        const documentLinks = readTopologyDocumentLinks(topology);

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

        const topologyNodes: GraphNode[] = [...entityNodes, ...documentNodes];

        const relationEdges = relations
          .map((r) => {
            if (!r.subjectEntityId || !r.objectEntityId) return null;
            return {
              id: r.relationId ?? r.id ?? '',
              sourceId: r.subjectEntityId,
              targetId: r.objectEntityId,
              label: r.predicate ?? '',
              weight: r.supportCount ?? 1,
            };
          })
          .filter((edge): edge is GraphEdgeData => edge !== null);

        const documentEdges = documentLinks.map((link) => ({
          id: `dl-${link.documentId}-${link.targetNodeId}`,
          sourceId: link.documentId,
          targetId: link.targetNodeId,
          label: 'supports',
          weight: link.supportCount ?? 1,
        }));

        const topologyEdges = [...relationEdges, ...documentEdges];

        edgesRef.current = topologyEdges;
        const recommendedLayout = recommendGraphLayout(topologyNodes, topologyEdges);

        const topologyStatus = topology.status ?? (topologyNodes.length > 0 ? 'ready' : 'empty');
        const topologyMeta: GraphMetadata = {
          nodeCount: topologyNodes.length,
          edgeCount: topologyEdges.length,
          hiddenDisconnectedCount: 0,
          status: topologyStatus,
          convergenceStatus: topology.convergenceStatus ?? 'current',
          recommendedLayout,
        };

        setAllNodes(topologyNodes);
        setGraphMeta(topologyMeta);
        setGraphStatus(topologyMeta.status);
        setLayout(recommendedLayout);
      } catch (err: unknown) {
        if (cancelled) return;
        setLoadError(errorMessage(err, 'Failed to load graph'));
        setGraphStatus('failed');
      }
    })();

    return () => { cancelled = true; };
  }, [activeLibrary]);

  // Reset the "expand summary" toggle whenever a new node is selected so the
  // inspector starts collapsed for every fresh selection.
  useEffect(() => {
    setSummaryExpanded(false);
  }, [selectedNode]);

  // Fetch node detail when selected — different API for entities vs documents
  useEffect(() => {
    if (!activeLibrary || !selectedNode) {
      setSelectedDetail(null);
      return;
    }
    const basic = allNodes.find(n => n.id === selectedNode) ?? null;
    setSelectedDetail(basic);
    setDetailLoading(true);

    let cancelled = false;

    if (basic?.type === 'document') {
      // For documents, fetch document detail from content API
      documentsApi.get(selectedNode)
        .then((doc) => {
          if (cancelled) return;
          // Web-captured documents store their URL path basename as
          // `fileName` (e.g. "viewpage.action" for Confluence pages), which
          // is meaningless to a human. Prefer the full `source_uri` for
          // `web_page` documents so the inspector header shows the actual
          // page URL instead of a noisy CGI handler name.
          const rev = (doc.activeRevision ?? doc.active_revision) as
            | {
                mime_type?: string;
                byte_size?: number;
                revision_number?: number;
                content_source_kind?: string;
                source_uri?: string;
              }
            | undefined;
          const isWebPage = rev?.content_source_kind === 'web_page';
          const fileNameLabel =
            typeof doc.fileName === 'string' ? doc.fileName : undefined;
          const label =
            (isWebPage ? rev?.source_uri : undefined) ?? fileNameLabel ?? basic.label;
          const enriched: GraphNode = {
            id: selectedNode,
            label,
            type: 'document',
            summary: resolveDocumentSummary(doc as Record<string, unknown>) ?? basic.summary,
            edgeCount: basic.edgeCount,
            properties: {},
            sourceDocumentIds: [],
          };
          if (rev?.mime_type) enriched.properties['format'] = rev.mime_type;
          if (rev?.byte_size != null) enriched.properties['size'] = `${(rev.byte_size / 1024).toFixed(1)} KB`;
          if (rev?.revision_number != null) enriched.properties['revision'] = String(rev.revision_number);
          enriched.properties['state'] = doc.readinessSummary?.readinessKind ?? 'unknown';
          enriched.properties['activity'] = doc.readinessSummary?.activityStatus ?? 'unknown';
          if (doc.readinessSummary?.graphCoverageKind) enriched.properties['graph coverage'] = doc.readinessSummary.graphCoverageKind;
          setSelectedDetail(enriched);
        })
        .catch((err) => {
          console.warn('failed to load entity detail, falling back to basic', err);
          if (!cancelled) setSelectedDetail(basic);
        })
        .finally(() => { if (!cancelled) setDetailLoading(false); });
      return () => { cancelled = true; };
    }

    // For entities/topics, use the knowledge entity API
    knowledgeApi.getEntity(activeLibrary.id, selectedNode)
      .then((rawDetail) => {
        if (cancelled) return;
        const detail = rawDetail as RawKnowledgeEntityDetail;
        const entity: RawKnowledgeEntity = detail.entity ?? (detail as RawKnowledgeEntity);
        const canonicalType = mapNodeType(entity.entityType ?? entity.nodeType);
        const rawType = (entity.entityType ?? '').toLowerCase();
        const resolvedSubType =
          entity.entitySubType ??
          basic?.subType ??
          (rawType !== canonicalType ? rawType : undefined);
        const enriched: GraphNode = {
          id: entity.entityId ?? entity.id ?? selectedNode,
          label: entity.canonicalLabel ?? entity.label ?? basic?.label ?? '',
          type: canonicalType,
          subType: resolvedSubType,
          summary: entity.summary ?? basic?.summary ?? undefined,
          edgeCount: entity.supportCount ?? basic?.edgeCount ?? 0,
          properties: {},
          sourceDocumentIds: [],
        };
        if (entity.entityType) enriched.properties['type'] = entity.entityType;
        if (entity.confidence != null) enriched.properties['confidence'] = String(Math.round(entity.confidence * 100)) + '%';
        if (entity.supportCount != null) enriched.properties['support count'] = String(entity.supportCount);
        if (entity.entityState) enriched.properties['state'] = entity.entityState;
        if (entity.aliases?.length) enriched.properties['aliases'] = entity.aliases.join(', ');
        if (detail.selectedNode?.relatedNodes) {
          enriched.sourceDocumentIds = (detail.selectedNode.supportingDocuments ?? []).map((d) => d.documentId);
        }
        setSelectedDetail(enriched);
      })
      .catch((err: unknown) => {
        console.error("Entity detail failed:", err);
        toast.error(errorMessage(err, 'Failed to load entity details'));
      })
      .finally(() => {
        if (!cancelled) setDetailLoading(false);
      });

    return () => { cancelled = true; };
  }, [activeLibrary, selectedNode, allNodes]);

  const filteredNodes = useMemo(() => allNodes.filter(n => {
    if (hiddenTypes.has(n.type)) return false;
    if (hiddenSubTypes.has(subtypeFilterKey(n.type, n.subType))) return false;
    if (searchQuery && !n.label.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  }), [allNodes, hiddenTypes, hiddenSubTypes, searchQuery]);

  const effectiveLayout = layout;
  const activeLayoutOption = GRAPH_LAYOUT_OPTIONS.find((option) => option.id === layout) ?? GRAPH_LAYOUT_OPTIONS[0];
  const recommendedLayout =
    graphMeta?.recommendedLayout && isGraphLayoutType(graphMeta.recommendedLayout)
      ? graphMeta.recommendedLayout
      : null;

  // Compute type → subtype breakdown for legend.
  const typeLegend = useMemo(() => {
    const map = new Map<string, { count: number; subs: Map<string, number>; noSubtypeCount: number }>();
    for (const n of allNodes) {
      let entry = map.get(n.type);
      if (!entry) { entry = { count: 0, subs: new Map(), noSubtypeCount: 0 }; map.set(n.type, entry); }
      entry.count++;
      if (n.subType && n.subType.trim().length > 0) {
        entry.subs.set(n.subType, (entry.subs.get(n.subType) ?? 0) + 1);
      } else {
        entry.noSubtypeCount += 1;
      }
    }
    return map;
  }, [allNodes]);

  const selected = selectedDetail ?? allNodes.find(n => n.id === selectedNode) ?? null;

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header"><h1 className="text-lg font-bold tracking-tight">{t('graph.title')}</h1></div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <Share2 className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('graph.noLibrary')}</h2>
          <p className="text-sm text-muted-foreground mt-2">{t('graph.noLibraryDesc')}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 min-h-0 flex flex-col overflow-hidden">
      {/* Toolbar */}
      <div className="px-4 py-2.5 border-b flex items-center gap-2 flex-wrap" style={{
        background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--background)))',
      }}>
        <div className="relative min-w-[180px]">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input className="h-8 pl-8 text-xs" placeholder={t('graph.searchPlaceholder')} value={searchQuery} onChange={e => setSearchQuery(e.target.value)} />
        </div>

        {/* Type filter moved to clickable legend below */}

        <div className="flex items-center gap-1 rounded-xl border border-border/60 bg-card/80 p-1 shadow-soft">
          {GRAPH_LAYOUT_OPTIONS.map((option) => {
            const isActive = layout === option.id;
            const Icon = GRAPH_LAYOUT_ICONS[option.iconKey];
            return (
              <button
                key={option.id}
                onClick={() => setLayout(option.id)}
                className={`flex h-8 w-8 items-center justify-center rounded-lg transition-all ${
                  isActive
                    ? 'bg-primary text-primary-foreground shadow-sm'
                    : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                }`}
                title={t(option.labelKey)}
                aria-label={t(option.labelKey)}
              >
                <Icon className="h-4 w-4" />
              </button>
            );
          })}
        </div>

        <div className="hidden xl:flex xl:min-w-[240px] xl:flex-col">
          <span className="text-xs font-semibold text-foreground">{t(activeLayoutOption.labelKey)}</span>
          <span className="text-xs text-muted-foreground">{t(activeLayoutOption.descriptionKey)}</span>
        </div>

        {recommendedLayout && layout !== recommendedLayout && (
          <button
            type="button"
            onClick={() => setLayout(recommendedLayout)}
            className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full border border-amber-300/70 bg-amber-50/90 px-3 text-xs font-medium text-amber-950 shadow-sm transition-colors hover:bg-amber-100"
          >
            <AlertTriangle className="h-3.5 w-3.5 text-amber-600" />
            <span className="text-muted-foreground">{t('graph.recommended')}</span>
            <span className="font-semibold text-primary">{t(`graph.layouts.${recommendedLayout}`)}</span>
          </button>
        )}

        {/* Sigma.js handles zoom via mouse wheel / pinch */}

        {hasActiveGraphState && (
          <button className="h-7 px-2.5 text-xs flex items-center gap-1.5 rounded-lg hover:bg-muted transition-all duration-200 font-semibold" onClick={resetGraphView}>
            <X className="h-3.5 w-3.5" /> {t('graph.clear')}
          </button>
        )}

        <div className="ml-auto flex items-center gap-3 text-xs text-muted-foreground">
          <span className="tabular-nums font-semibold">{graphMeta?.nodeCount ?? 0} {t('graph.nodes')}</span>
          <span className="tabular-nums font-semibold">{graphMeta?.edgeCount ?? 0} {t('graph.edges')}</span>
          {(graphMeta?.hiddenDisconnectedCount ?? 0) > 0 && <span className="tabular-nums">{graphMeta!.hiddenDisconnectedCount} {t('graph.hidden')}</span>}
          <span className={`status-badge ${graphStatus === 'ready' ? 'status-ready' : graphStatus === 'partial' ? 'status-warning' : graphStatus === 'failed' ? 'status-failed' : 'status-processing'}`}>
            {graphStatus}
          </span>
        </div>
      </div>

      <div className="flex-1 min-h-0 relative overflow-hidden">
        <div className="absolute inset-0">
          {graphStatus === 'building' || graphStatus === 'rebuilding' ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <Loader2 className="h-8 w-8 animate-spin text-primary/60 mb-3" />
              <p className="text-sm font-semibold text-muted-foreground">{t('graph.loading')}</p>
            </div>
          ) : loadError ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <AlertTriangle className="h-7 w-7 text-status-failed" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{t('graph.failedToLoad')}</h2>
              <p className="text-sm text-muted-foreground mt-2 max-w-sm text-center">{loadError}</p>
            </div>
          ) : filteredNodes.length === 0 ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <Share2 className="h-7 w-7 text-muted-foreground" />
              </div>
              <h2 className="text-base font-bold tracking-tight">
                {allNodes.length === 0 ? t('graph.noGraph') : t('graph.noMatchingNodes')}
              </h2>
              <p className="text-sm text-muted-foreground mt-2 max-w-sm text-center">
                {allNodes.length === 0 ? t('graph.noGraphDesc') : t('graph.noMatchingNodesDesc')}
              </p>
              {allNodes.length === 0 && (
                <Button variant="outline" size="sm" className="mt-4" onClick={() => navigate('/documents')}>
                  <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('graph.goToDocuments')}
                </Button>
              )}
            </div>
          ) : (
            <Suspense fallback={<div className="flex-1 flex items-center justify-center"><Loader2 className="h-6 w-6 animate-spin" /></div>}>
              <SigmaGraph
                nodes={filteredNodes}
                edges={edgesRef.current}
                selectedId={selectedNode}
                onSelect={setSelectedNode}
                layout={effectiveLayout}
              />
            </Suspense>
          )}

          {/* Legend toggle button — always visible */}
          {!legendOpen && (
            <button
              onClick={() => setLegendOpen(true)}
              className="absolute top-3 left-3 glass-panel rounded-xl p-2 shadow-lifted cursor-pointer hover:bg-white/10 transition-all"
              title={t('graph.showLegend')}
            >
              <Layers className="h-4 w-4 text-muted-foreground" />
            </button>
          )}

          {/* Vertical legend — left side */}
          {legendOpen && (
            <div className="absolute top-3 left-3 bottom-3 max-h-[calc(100%-24px)] overflow-y-auto text-xs glass-panel rounded-xl shadow-lifted min-w-[150px] max-w-[250px] flex flex-col">
              {/* Legend header with controls */}
              <div className="flex items-center gap-1 px-3 py-2 border-b border-white/10">
                <span className="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider flex-1">{t('graph.legend')}</span>
                <button
                  onClick={() => { setHiddenTypes(new Set()); setHiddenSubTypes(new Set()); }}
                  className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
                  title={t('graph.showAll')}
                >
                  <Eye className="h-3.5 w-3.5 text-muted-foreground" />
                </button>
                <button
                  onClick={() => {
                    setHiddenTypes(prev => {
                      const allTypes = Object.keys(GRAPH_NODE_COLORS);
                      const next = new Set<string>();
                      for (const tp of allTypes) {
                        if (!prev.has(tp)) next.add(tp);
                      }
                      return next;
                    });
                  }}
                  className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
                  title={t('graph.invert')}
                >
                  <RotateCcw className="h-3.5 w-3.5 text-muted-foreground" />
                </button>
                <button
                  onClick={() => setLegendOpen(false)}
                  className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
                  title={t('graph.hideLegend')}
                >
                  <EyeOff className="h-3.5 w-3.5 text-muted-foreground" />
                </button>
              </div>

              {/* Type list */}
              <div className="px-2 py-1.5 flex-1 overflow-y-auto">
                {Object.entries(GRAPH_NODE_COLORS).map(([type, color]) => {
                  const isHidden = hiddenTypes.has(type);
                  const stats = typeLegend.get(type);
                  const count = stats?.count ?? 0;
                  if (count === 0 && type !== 'document') return null;
                  const realSubs = stats?.subs ? Array.from(stats.subs.entries()).sort((a, b) => b[1] - a[1]) : [];
                  const subs = stats && stats.noSubtypeCount > 0 && realSubs.length > 0
                    ? [...realSubs, [NO_SUBTYPE_KEY, stats.noSubtypeCount] as const]
                    : realSubs;
                  const isSubtypeGroupExpanded = expandedSubtypeGroups.has(type);
                  const visibleSubs = isSubtypeGroupExpanded ? subs : subs.slice(0, SUBTYPE_PREVIEW_LIMIT);
                  const hiddenSubtypeCount = Math.max(0, subs.length - SUBTYPE_PREVIEW_LIMIT);
                  return (
                    <div key={type} className={`mb-0.5 ${isHidden ? 'opacity-35' : ''}`}>
                      <button
                        className={`flex items-center gap-1.5 w-full px-2 py-1 rounded-md transition-all cursor-pointer ${isHidden ? 'line-through' : 'hover:bg-white/10'}`}
                        onClick={(e) => {
                          if (e.ctrlKey || e.metaKey) {
                            // Ctrl/Cmd+Click: toggle single type
                            setHiddenTypes(prev => {
                              const next = new Set(prev);
                              if (next.has(type)) next.delete(type);
                              else next.add(type);
                              return next;
                            });
                          } else {
                            // Click: isolate (show only this) or reset
                            const allTypes = Object.keys(GRAPH_NODE_COLORS);
                            const othersHidden = allTypes.filter(t => t !== type).every(t => hiddenTypes.has(t));
                            if (othersHidden && !hiddenTypes.has(type)) {
                              setHiddenTypes(new Set());
                              setHiddenSubTypes(new Set());
                            } else {
                              setHiddenTypes(new Set(allTypes.filter(t => t !== type)));
                              setHiddenSubTypes(new Set());
                            }
                          }
                        }}
                        title={t(`graph.nodeTypes.${type}`)}
                      >
                        <span className="w-2.5 h-2.5 rounded-full shrink-0" style={{ background: color }} />
                        <span className="font-semibold truncate">{t(`graph.nodeTypes.${type}`)}</span>
                        <span className="ml-auto tabular-nums text-muted-foreground">{count}</span>
                      </button>
                      {subs.length > 0 && !isHidden && (
                        <div className="pl-6 pr-1 mt-0.5 mb-1">
                          <div className="flex flex-wrap gap-x-1.5 gap-y-0.5">
                          {visibleSubs.map(([sub, subCount]) => {
                            const subKey = `${type}:${sub}`;
                            const isSubHidden = hiddenSubTypes.has(subKey);
                            return (
                              <button
                                key={sub}
                                className={`text-[10px] whitespace-nowrap cursor-pointer rounded px-1 py-0.5 transition-colors ${isSubHidden ? 'opacity-35 line-through text-muted-foreground' : 'text-muted-foreground hover:bg-white/10'}`}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  if (e.ctrlKey || e.metaKey) {
                                    // Ctrl/Cmd+Click: toggle single sub-type
                                    setHiddenSubTypes(prev => {
                                      const next = new Set(prev);
                                      if (next.has(subKey)) next.delete(subKey);
                                      else next.add(subKey);
                                      return next;
                                    });
                                  } else {
                                    // Click: isolate sub-type or reset
                                    const siblingKeys = subs.map(([s]) => `${type}:${s}`);
                                    if (siblingKeys.length === 1) {
                                      setHiddenSubTypes(prev => {
                                        const next = new Set(prev);
                                        if (next.has(subKey)) next.delete(subKey);
                                        else next.add(subKey);
                                        return next;
                                      });
                                      return;
                                    }
                                    const othersHidden = siblingKeys.filter(k => k !== subKey).every(k => hiddenSubTypes.has(k));
                                    if (othersHidden && !hiddenSubTypes.has(subKey)) {
                                      setHiddenSubTypes(prev => {
                                        const next = new Set(prev);
                                        for (const k of siblingKeys) next.delete(k);
                                        return next;
                                      });
                                    } else {
                                      setHiddenSubTypes(prev => {
                                        const next = new Set(prev);
                                        for (const k of siblingKeys) {
                                          if (k === subKey) next.delete(k);
                                          else next.add(k);
                                        }
                                        return next;
                                      });
                                    }
                                  }
                                }}
                                title={subtypeLegendLabel(t, sub)}
                              >
                                <span className="inline-block w-1.5 h-1.5 rounded-full mr-0.5 align-middle" style={{ background: color, opacity: 0.6 }} />
                                {subtypeLegendLabel(t, sub)} <span className="tabular-nums">{subCount}</span>
                              </button>
                            );
                          })}
                          </div>
                          {subs.length > SUBTYPE_PREVIEW_LIMIT && (
                            <button
                              type="button"
                              className="mt-1 inline-flex h-6 items-center rounded-md px-1.5 text-[10px] font-medium text-muted-foreground transition-colors hover:bg-white/10 hover:text-foreground"
                              onClick={(e) => {
                                e.stopPropagation();
                                setExpandedSubtypeGroups((prev) => {
                                  const next = new Set(prev);
                                  if (next.has(type)) next.delete(type);
                                  else next.add(type);
                                  return next;
                                });
                              }}
                            >
                              {isSubtypeGroupExpanded
                                ? t('graph.hideSubTypes')
                                : t('graph.showAllSubTypes', { count: hiddenSubtypeCount })}
                            </button>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </div>

        {selected && (() => {
          // Build ACTUALLY connected nodes using adjacency map
          const connectedIds = edgesRef.current
            .filter(e => e.sourceId === selected.id || e.targetId === selected.id)
            .map(e => e.sourceId === selected.id ? e.targetId : e.sourceId);
          const connectedNodes = connectedIds
            .map(id => allNodes.find(n => n.id === id))
            .filter((n): n is GraphNode => n != null)
            .slice(0, 20);
          const connectedDocs = connectedNodes.filter(n => n.type === 'document');
          const connectedEntities = connectedNodes.filter(n => n.type === 'entity');
          const connectedConcepts = connectedNodes.filter(n => n.type === 'concept');

          return (
            <div className="absolute top-0 right-0 z-20 h-full w-[24rem] overflow-y-auto border-l bg-card shadow-xl animate-slide-in-right lg:w-[30rem] xl:w-[34rem]">
              <div className="flex items-start gap-2 border-b p-4">
                <h3
                  className="min-w-0 flex-1 text-[15px] font-bold tracking-tight leading-5 text-foreground [overflow-wrap:anywhere]"
                  title={selected.label}
                >
                  {selected.label}
                </h3>
                <div className="flex items-center gap-1">
                  {detailLoading && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />}
                  <button onClick={() => setSelectedNode(null)} className="p-1.5 rounded-lg hover:bg-muted transition-colors" aria-label={t('common.close')}><X className="h-4 w-4" /></button>
                </div>
              </div>
              <div className="p-4 space-y-4">
                {/* Type & connections header */}
                <div className="flex items-center gap-2.5">
                  <span className="w-3 h-3 rounded-full" style={{ background: GRAPH_NODE_COLORS[selected.type] }} />
                  <div className="flex flex-col">
                    <span className="text-sm font-semibold capitalize">{t(`graph.nodeTypes.${selected.type}`)}</span>
                    {selected.subType && (
                      <span className="text-[11px] text-muted-foreground capitalize">{selected.subType}</span>
                    )}
                  </div>
                  <span className="text-xs text-muted-foreground ml-auto tabular-nums font-medium">{connectedIds.length} {t('graph.connections')}</span>
                </div>

                {selected.type !== 'document' && (
                  <div className="flex items-center justify-between text-xs">
                    <span className="text-muted-foreground">{t('graph.subType')}</span>
                    <span className="font-medium text-foreground">{selected.subType ?? '—'}</span>
                  </div>
                )}

                {/* Summary — truncated by default, expandable on click. The
                    full backend summary can be ~1000 chars and crowds the
                    inspector; the short form keeps the panel scannable while
                    the user can drill in when they actually want the full
                    text. */}
                {selected.summary && (() => {
                  const SUMMARY_PREVIEW_CHARS = 280;
                  const fullSummary = selected.summary.trim();
                  const isLong = fullSummary.length > SUMMARY_PREVIEW_CHARS;
                  const visible = !isLong || summaryExpanded
                    ? fullSummary
                    : `${fullSummary.slice(0, SUMMARY_PREVIEW_CHARS).trimEnd()}…`;
                  return (
                    <div>
                      <div className="section-label mb-1">{t('graph.summary')}</div>
                      <p className="text-sm leading-relaxed text-muted-foreground whitespace-pre-wrap [overflow-wrap:anywhere]">
                        {visible}
                      </p>
                      {isLong && (
                        <button
                          type="button"
                          onClick={() => setSummaryExpanded(prev => !prev)}
                          className="mt-1 text-xs font-semibold text-primary hover:underline"
                        >
                          {summaryExpanded ? t('graph.summaryCollapse') : t('graph.summaryExpand')}
                        </button>
                      )}
                    </div>
                  );
                })()}

                {/* Properties */}
                {Object.keys(selected.properties).length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.properties')}</div>
                    <div className="space-y-1">
                      {Object.entries(selected.properties).map(([k, v]) => (
                        <div key={k} className="grid grid-cols-[80px_minmax(0,1fr)] items-start gap-x-3 text-xs">
                          <span className="pt-0.5 text-muted-foreground capitalize">{k}</span>
                          <span className="min-w-0 text-right font-semibold leading-tight text-foreground [overflow-wrap:anywhere]">
                            {v}
                          </span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* Actions */}
                <div className="flex gap-2">
                  {selected.type === 'document' && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="text-xs h-7"
                      onClick={() => navigate(`/documents?documentId=${encodeURIComponent(selected.id)}`)}
                    >
                      <FileText className="h-3 w-3 mr-1" /> {t('graph.viewDocument')}
                    </Button>
                  )}
                </div>

                {/* Connected Documents */}
                {connectedDocs.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.sourceDocuments')} ({connectedDocs.length})</div>
                    <div className="space-y-0.5">
                      {connectedDocs.map(n => {
                        const compactLabel = compactText(n.label, 48);
                        return (
                          <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                            <span className="w-2 h-2 rounded-full shrink-0" style={{ background: GRAPH_NODE_COLORS.document }} />
                            <span className="truncate font-medium" title={compactLabel.fullText}>{compactLabel.text}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                )}

                {/* Connected Entities */}
                {connectedEntities.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedEntities')} ({connectedEntities.length})</div>
                    <div className="space-y-0.5">
                      {connectedEntities.slice(0, 15).map(n => {
                        const compactLabel = compactText(n.label, 48);
                        return (
                          <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                            <span className="w-2 h-2 rounded-full shrink-0" style={{ background: GRAPH_NODE_COLORS.entity }} />
                            <span className="truncate" title={compactLabel.fullText}>{compactLabel.text}</span>
                            {n.edgeCount > 0 && <span className="text-[10px] text-muted-foreground ml-auto tabular-nums">{n.edgeCount}</span>}
                          </button>
                        );
                      })}
                      {connectedEntities.length > 15 && <span className="text-xs text-muted-foreground pl-6">+{connectedEntities.length - 15} more</span>}
                    </div>
                  </div>
                )}

                {/* Connected Concepts */}
                {connectedConcepts.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedConcepts')} ({connectedConcepts.length})</div>
                    <div className="space-y-0.5">
                      {connectedConcepts.slice(0, 10).map(n => {
                        const compactLabel = compactText(n.label, 48);
                        return (
                          <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                            <span className="w-2 h-2 rounded-full shrink-0" style={{ background: GRAPH_NODE_COLORS.concept }} />
                            <span className="truncate" title={compactLabel.fullText}>{compactLabel.text}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                )}

                {connectedNodes.length === 0 && !detailLoading && (
                  <p className="text-xs text-muted-foreground">{t('graph.noConnections')}</p>
                )}
              </div>
            </div>
          );
        })()}
      </div>
    </div>
  );
}
