import { useState, useEffect, useRef, useMemo, lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { useNavigate } from 'react-router-dom';
import { documentsApi, knowledgeApi } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Search, X, Loader2,
  FileText, Share2, AlertTriangle,
  Eye, EyeOff, RotateCcw, Layers,
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


const SigmaGraph = lazy(() => import('@/components/SigmaGraph'));

const LAYOUTS = ['cloud', 'circle', 'rings', 'lanes', 'clusters', 'islands', 'spiral'] as const;
type LayoutType = typeof LAYOUTS[number];

const NODE_COLORS: Record<string, string> = {
  document: '#3b82f6',      // blue
  person: '#ec4899',        // pink
  organization: '#64748b',  // slate
  location: '#84cc16',      // lime
  event: '#f43f5e',         // rose
  artifact: '#06b6d4',      // cyan
  natural: '#22c55e',       // green
  process: '#a855f7',       // purple
  concept: '#f59e0b',       // amber
  attribute: '#0ea5e9',     // sky
  entity: '#78716c',        // stone
};

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


export default function GraphPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  // Edges from entities/relations fallback
  const edgesRef = useRef<{ id: string; sourceId: string; targetId: string; label: string; weight: number }[]>([]);

  // API state
  const [allNodes, setAllNodes] = useState<GraphNode[]>([]);
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null);
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building');
  const [loadError, setLoadError] = useState<string | null>(null);

  // Node detail state (from entity detail endpoint)
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [selectedDetail, setSelectedDetail] = useState<GraphNode | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  // UI controls
  const [searchQuery, setSearchQuery] = useState('');
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  const [hiddenSubTypes, setHiddenSubTypes] = useState<Set<string>>(new Set());
  const [layout, setLayout] = useState<LayoutType>('cloud');
  const [legendOpen, setLegendOpen] = useState(true);

  // Fetch graph workbench data, falling back to entities+relations endpoints
  useEffect(() => {
    if (!activeLibrary) return;
    let cancelled = false;
    setGraphStatus('building');
    setLoadError(null);
    setAllNodes([]);
    setGraphMeta(null);
    setSelectedNode(null);
    setSelectedDetail(null);

    // Load graph data from fast individual endpoints (not slow graph-workbench)
    Promise.all([
      knowledgeApi.listEntities(activeLibrary.id),
      knowledgeApi.listRelations(activeLibrary.id),
      knowledgeApi.listDocuments(activeLibrary.id),
      knowledgeApi.getGraphTopology(activeLibrary.id).catch((err) => {
        console.warn('failed to load graph topology', err);
        return null;
      }),
    ]).then(([entitiesRes, relationsRes, documentsRes, topologyRes]) => {
          if (cancelled) return;

          const entities: RawKnowledgeEntity[] = Array.isArray(entitiesRes)
            ? (entitiesRes as RawKnowledgeEntity[])
            : ((entitiesRes.items ?? []) as RawKnowledgeEntity[]);
          const relations: RawKnowledgeRelation[] = Array.isArray(relationsRes)
            ? (relationsRes as RawKnowledgeRelation[])
            : ((relationsRes.items ?? []) as RawKnowledgeRelation[]);
          const documents: RawKnowledgeDocument[] = Array.isArray(documentsRes)
            ? (documentsRes as RawKnowledgeDocument[])
            : ((documentsRes.items ?? documentsRes.documents ?? []) as RawKnowledgeDocument[]);
          const documentLinks: RawGraphDocumentLink[] =
            (topologyRes?.documentLinks as RawGraphDocumentLink[] | undefined) ?? [];

          // Pre-compute per-document edge counts from topology links
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
              summary: d.document_state ?? undefined,
              edgeCount: docEdgeCounts.get(docId) ?? 0,
              properties: {},
              sourceDocumentIds: [],
            };
          });

          const fallbackNodes: GraphNode[] = [...entityNodes, ...documentNodes];

          const relationEdges = relations.map((r) => ({
            id: r.relationId ?? r.id ?? '',
            sourceId: r.subjectEntityId,
            targetId: r.objectEntityId,
            label: r.predicate ?? '',
            weight: r.supportCount ?? 1,
          }));

          const documentEdges = documentLinks.map((link) => ({
            id: `dl-${link.documentId}-${link.targetNodeId}`,
            sourceId: link.documentId,
            targetId: link.targetNodeId,
            label: 'supports',
            weight: link.supportCount ?? 1,
          }));

          const fallbackEdges = [...relationEdges, ...documentEdges];

          // Store edges on the module level so GraphCanvas can use them
          edgesRef.current = fallbackEdges;

          const fallbackMeta: GraphMetadata = {
            nodeCount: fallbackNodes.length,
            edgeCount: fallbackEdges.length,
            hiddenDisconnectedCount: 0,
            status: fallbackNodes.length > 0 ? 'ready' : 'empty',
            convergenceStatus: 'current',
            recommendedLayout: undefined,
          };

          setAllNodes(fallbackNodes);
          setGraphMeta(fallbackMeta);
          setGraphStatus(fallbackMeta.status);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setLoadError(errorMessage(err, 'Failed to load graph'));
        setGraphStatus('failed');
      });

    return () => { cancelled = true; };
  }, [activeLibrary]);

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
          const enriched: GraphNode = {
            id: selectedNode,
            label: (typeof doc.fileName === 'string' ? doc.fileName : undefined) ?? basic.label,
            type: 'document',
            summary: doc.readinessSummary?.readinessKind ?? basic.summary,
            edgeCount: basic.edgeCount,
            properties: {},
            sourceDocumentIds: [],
          };
          const rev = (doc.activeRevision ?? doc.active_revision) as
            | { mime_type?: string; byte_size?: number; revision_number?: number }
            | undefined;
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
        const enriched: GraphNode = {
          id: entity.entityId ?? entity.id ?? selectedNode,
          label: entity.canonicalLabel ?? entity.label ?? basic?.label ?? '',
          type: canonicalType,
          subType: rawType !== canonicalType ? rawType : undefined,
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
    if (n.subType && hiddenSubTypes.has(`${n.type}:${n.subType}`)) return false;
    if (searchQuery && !n.label.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  }), [allNodes, hiddenTypes, hiddenSubTypes, searchQuery]);

  const effectiveLayout = layout;

  // Compute type → { count, subTypes: { name, count }[] } for legend
  const typeLegend = useMemo(() => {
    const map = new Map<string, { count: number; subs: Map<string, number> }>();
    for (const n of allNodes) {
      let entry = map.get(n.type);
      if (!entry) { entry = { count: 0, subs: new Map() }; map.set(n.type, entry); }
      entry.count++;
      if (n.subType) {
        entry.subs.set(n.subType, (entry.subs.get(n.subType) ?? 0) + 1);
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

        <div className="flex items-center gap-0.5 bg-muted/50 rounded-lg p-0.5">
          {LAYOUTS.map(l => {
            const icons: Record<string, string> = { cloud: '⬡', circle: '○', rings: '◎', lanes: '≡', clusters: '⬢', islands: '◇', spiral: '✺' };
            const isActive = layout === l;
            return (
              <button
                key={l}
                onClick={() => setLayout(l)}
                className={`px-2 py-1 text-sm rounded-md transition-all font-mono ${isActive ? 'bg-primary text-primary-foreground shadow-sm font-bold' : 'text-muted-foreground hover:text-foreground hover:bg-muted'}`}
                title={t(`graph.layouts.${l}`)}
              >
                {icons[l] || l}
              </button>
            );
          })}
        </div>

        {/* Sigma.js handles zoom via mouse wheel / pinch */}

        {selectedNode && (
          <button className="h-7 px-2.5 text-xs flex items-center gap-1.5 rounded-lg hover:bg-muted transition-all duration-200 font-semibold" onClick={() => setSelectedNode(null)}>
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
                hiddenTypes={hiddenTypes}
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
                      const allTypes = Object.keys(NODE_COLORS);
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
                {Object.entries(NODE_COLORS).map(([type, color]) => {
                  const isHidden = hiddenTypes.has(type);
                  const stats = typeLegend.get(type);
                  const count = stats?.count ?? 0;
                  if (count === 0 && type !== 'document') return null;
                  const subs = stats?.subs ? Array.from(stats.subs.entries()).sort((a, b) => b[1] - a[1]) : [];
                  return (
                    <div key={type} className={`mb-0.5 ${isHidden ? 'opacity-35' : ''}`}>
                      <button
                        className={`flex items-center gap-1.5 w-full px-2 py-1 rounded-md transition-all cursor-pointer ${isHidden ? 'line-through' : 'hover:bg-white/10'}`}
                        onClick={() => {
                          setHiddenTypes(prev => {
                            const next = new Set(prev);
                            if (next.has(type)) next.delete(type);
                            else next.add(type);
                            return next;
                          });
                        }}
                        title={t(`graph.nodeTypes.${type}`)}
                      >
                        <span className="w-2.5 h-2.5 rounded-full shrink-0" style={{ background: color }} />
                        <span className="font-semibold truncate">{t(`graph.nodeTypes.${type}`)}</span>
                        <span className="ml-auto tabular-nums text-muted-foreground">{count}</span>
                      </button>
                      {subs.length > 0 && !isHidden && (
                        <div className="flex flex-wrap gap-x-1.5 gap-y-0.5 pl-6 pr-1 mt-0.5 mb-1">
                          {subs.slice(0, 12).map(([sub, subCount]) => {
                            const subKey = `${type}:${sub}`;
                            const isSubHidden = hiddenSubTypes.has(subKey);
                            return (
                              <button
                                key={sub}
                                className={`text-[10px] whitespace-nowrap cursor-pointer rounded px-1 py-0.5 transition-colors ${isSubHidden ? 'opacity-35 line-through text-muted-foreground' : 'text-muted-foreground hover:bg-white/10'}`}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setHiddenSubTypes(prev => {
                                    const next = new Set(prev);
                                    if (next.has(subKey)) next.delete(subKey);
                                    else next.add(subKey);
                                    return next;
                                  });
                                }}
                                title={sub}
                              >
                                <span className="inline-block w-1.5 h-1.5 rounded-full mr-0.5 align-middle" style={{ background: color, opacity: 0.6 }} />
                                {sub} <span className="tabular-nums">{subCount}</span>
                              </button>
                            );
                          })}
                          {subs.length > 12 && (
                            <span className="text-[10px] text-muted-foreground px-1">+{subs.length - 12}</span>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {graphMeta?.recommendedLayout && layout !== graphMeta.recommendedLayout && (
            <div className="absolute top-3 left-3 text-xs glass-panel rounded-xl px-4 py-2.5 shadow-lifted flex items-center gap-1.5">
              <AlertTriangle className="h-3 w-3 text-status-warning" />
              {t('graph.recommended')} <button className="font-bold text-primary hover:underline" onClick={() => setLayout(graphMeta.recommendedLayout as LayoutType)}>{graphMeta.recommendedLayout}</button>
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
            <div className="absolute top-0 right-0 h-full w-80 lg:w-96 bg-card border-l shadow-xl z-20 overflow-y-auto animate-slide-in-right">
              <div className="p-4 border-b flex items-center justify-between">
                <h3 className="text-sm font-bold truncate tracking-tight">{selected.label}</h3>
                <div className="flex items-center gap-1">
                  {detailLoading && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />}
                  <button onClick={() => setSelectedNode(null)} className="p-1.5 rounded-lg hover:bg-muted transition-colors" aria-label={t('common.close')}><X className="h-4 w-4" /></button>
                </div>
              </div>
              <div className="p-4 space-y-4">
                {/* Type & connections header */}
                <div className="flex items-center gap-2.5">
                  <span className="w-3 h-3 rounded-full" style={{ background: NODE_COLORS[selected.type] }} />
                  <div className="flex flex-col">
                    <span className="text-sm font-semibold capitalize">{t(`graph.nodeTypes.${selected.type}`)}</span>
                    {selected.subType && (
                      <span className="text-[11px] text-muted-foreground capitalize">{selected.subType}</span>
                    )}
                  </div>
                  <span className="text-xs text-muted-foreground ml-auto tabular-nums font-medium">{connectedIds.length} {t('graph.connections')}</span>
                </div>

                {/* Summary */}
                {selected.summary && (
                  <div><div className="section-label mb-1">{t('graph.summary')}</div><p className="text-sm leading-relaxed text-muted-foreground">{selected.summary}</p></div>
                )}

                {/* Properties */}
                {Object.keys(selected.properties).length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.properties')}</div>
                    <div className="space-y-1">
                      {Object.entries(selected.properties).map(([k, v]) => (
                        <div key={k} className="flex justify-between text-xs">
                          <span className="text-muted-foreground capitalize">{k}</span>
                          <span className="font-semibold text-foreground">{v}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* Actions */}
                <div className="flex gap-2">
                  {selected.type === 'document' && (
                    <Button variant="outline" size="sm" className="text-xs h-7" onClick={() => navigate(`/documents?highlight=${selected.id}`)}>
                      <FileText className="h-3 w-3 mr-1" /> {t('graph.viewDocument')}
                    </Button>
                  )}
                  <Button variant="outline" size="sm" className="text-xs h-7" onClick={() => {
                    setSearchQuery(selected.label);
                  }}>
                    <Search className="h-3 w-3 mr-1" /> {t('graph.findSimilar')}
                  </Button>
                  {searchQuery && (
                    <Button variant="ghost" size="sm" className="text-xs h-7" onClick={() => {
                      setSearchQuery('');
                      setHiddenTypes(new Set());
                      setHiddenSubTypes(new Set());
                    }}>
                      <X className="h-3 w-3 mr-1" /> {t('graph.resetFilter')}
                    </Button>
                  )}
                </div>

                {/* Connected Documents */}
                {connectedDocs.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.sourceDocuments')} ({connectedDocs.length})</div>
                    <div className="space-y-0.5">
                      {connectedDocs.map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.document }} />
                          <span className="truncate font-medium">{n.label}</span>
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {/* Connected Entities */}
                {connectedEntities.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedEntities')} ({connectedEntities.length})</div>
                    <div className="space-y-0.5">
                      {connectedEntities.slice(0, 15).map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.entity }} />
                          <span className="truncate">{n.label}</span>
                          {n.edgeCount > 0 && <span className="text-[10px] text-muted-foreground ml-auto tabular-nums">{n.edgeCount}</span>}
                        </button>
                      ))}
                      {connectedEntities.length > 15 && <span className="text-xs text-muted-foreground pl-6">+{connectedEntities.length - 15} more</span>}
                    </div>
                  </div>
                )}

                {/* Connected Concepts */}
                {connectedConcepts.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedConcepts')} ({connectedConcepts.length})</div>
                    <div className="space-y-0.5">
                      {connectedConcepts.slice(0, 10).map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.concept }} />
                          <span className="truncate">{n.label}</span>
                        </button>
                      ))}
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
