import { useCallback, useEffect, useMemo, useState, lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { useNavigate } from 'react-router-dom';
import {
  mapGraphDocumentDetail,
  mapGraphTopology,
  mapKnowledgeEntityDetail,
} from '@/adapters/graph';
import { errorMessage } from '@/lib/errorMessage';
import { documentsApi, knowledgeApi } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  GRAPH_LAYOUT_OPTIONS,
  isGraphLayoutType,
  type GraphLayoutType,
} from '@/components/graph/config';
import {
  Search,
  X,
  Loader2,
  FileText,
  Share2,
  AlertTriangle,
  PieChart,
  Rows3,
  Network,
  CircleDashed,
  Orbit,
} from 'lucide-react';
import type { GraphEdge, GraphMetadata, GraphNode, GraphStatus } from '@/types';
import { GraphInspector } from '@/components/graph/GraphInspector';
import { GraphLegend } from '@/components/graph/GraphLegend';
import { buildTypeLegend, subtypeFilterKey } from '@/components/graph/typeLegend';
import { useGraphAdjacency } from '@/components/graph/useGraphAdjacency';

const SigmaGraph = lazy(() => import('@/components/SigmaGraph'));

const GRAPH_LAYOUT_ICONS = {
  sectors: PieChart,
  bands: Rows3,
  components: Network,
  rings: CircleDashed,
  clusters: Orbit,
} as const;

export default function GraphPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  // Graph data
  const [allNodes, setAllNodes] = useState<GraphNode[]>([]);
  const [allEdges, setAllEdges] = useState<GraphEdge[]>([]);
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null);
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building');
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadProgress, setLoadProgress] = useState<{ nodes: number; total: number } | null>(null);

  // Selection state
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [selectedDetail, setSelectedDetail] = useState<GraphNode | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  // UI controls
  const [searchQuery, setSearchQuery] = useState('');
  // `debouncedSearchQuery` trails `searchQuery` by 250 ms so rapid typing
  // does not recompute `hiddenIds` on every keystroke. On a 100k-node graph
  // the filter scan is still fast, but the Sigma reducer refresh that
  // follows each change is not — debouncing keeps the UI glassy while the
  // user is still composing their query.
  const [debouncedSearchQuery, setDebouncedSearchQuery] = useState('');
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  const [hiddenSubTypes, setHiddenSubTypes] = useState<Set<string>>(new Set());
  const [layout, setLayout] = useState<GraphLayoutType>('bands');
  const [legendOpen, setLegendOpen] = useState(true);
  const [expandedSubtypeGroups, setExpandedSubtypeGroups] = useState<Set<string>>(new Set());

  useEffect(() => {
    const handle = setTimeout(() => setDebouncedSearchQuery(searchQuery), 250);
    return () => clearTimeout(handle);
  }, [searchQuery]);

  const hasActiveGraphFilters =
    searchQuery.trim().length > 0 || hiddenTypes.size > 0 || hiddenSubTypes.size > 0;
  const hasActiveGraphState = hasActiveGraphFilters || selectedNode !== null;

  // Canonical adjacency index — computed once per (nodes, edges). The inspector,
  // search, and any future neighbor lookup read from this in O(k) per query
  // instead of scanning every edge on each render.
  const adjacency = useGraphAdjacency(allNodes, allEdges);

  const handleSelectNode = useCallback((nextId: string | null) => {
    setSelectedNode(nextId);
    setSelectedDetail(null);
    setDetailLoading(nextId !== null);
  }, []);

  const resetGraphView = useCallback(() => {
    handleSelectNode(null);
    setSearchQuery('');
    setHiddenTypes(new Set());
    setHiddenSubTypes(new Set());
    setExpandedSubtypeGroups(new Set());
  }, [handleSelectNode]);

  // Fetch graph topology on library change. Uses cancellation guard so a
  // rapid library switch does not commit stale data.
  useEffect(() => {
    if (!activeLibrary) return;
    let cancelled = false;

    void (async () => {
      setGraphStatus('building');
      setLoadError(null);
      setAllNodes([]);
      setAllEdges([]);
      setGraphMeta(null);
      setLoadProgress({ nodes: 0, total: 0 });
      setSelectedNode(null);
      setSelectedDetail(null);
      setDetailLoading(false);
      setSearchQuery('');
      setHiddenTypes(new Set());
      setHiddenSubTypes(new Set());

      try {
        const topologyRes = await knowledgeApi.getGraphTopology(activeLibrary.id, {
          onProgress: (progress) => {
            if (cancelled) return;
            setLoadProgress({
              nodes: progress.nodesLoaded,
              total: progress.expectedNodes,
            });
          },
        });
        if (cancelled) return;
        const {
          nodes: topologyNodes,
          edges: topologyEdges,
          meta: topologyMeta,
        } = mapGraphTopology(topologyRes);
        const recommendedLayout =
          topologyMeta.recommendedLayout && isGraphLayoutType(topologyMeta.recommendedLayout)
            ? topologyMeta.recommendedLayout
            : 'bands';

        setAllNodes(topologyNodes);
        setAllEdges(topologyEdges);
        setGraphMeta(topologyMeta);
        setGraphStatus(topologyMeta.status);
        setLayout(recommendedLayout);
        setLoadProgress(null);
      } catch (err: unknown) {
        if (cancelled) return;
        setLoadError(errorMessage(err, 'Failed to load graph'));
        setGraphStatus('failed');
        setLoadProgress(null);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [activeLibrary]);

  // Fetch node detail when selected — different API for entities vs documents.
  // Depends only on the selection and the adjacency index (used to look up the
  // basic node by ID), not on the full `allNodes` array, so selection events
  // do not re-fire on unrelated topology state changes.
  useEffect(() => {
    if (!activeLibrary || !selectedNode) return;
    const basic = adjacency.nodeById.get(selectedNode) ?? null;
    if (!basic) return;

    let cancelled = false;

    if (basic.type === 'document') {
      documentsApi
        .get(selectedNode)
        .then((doc) => {
          if (cancelled) return;
          setSelectedDetail(mapGraphDocumentDetail(doc, basic, selectedNode));
        })
        .catch((err) => {
          console.warn('failed to load entity detail, falling back to basic', err);
          if (!cancelled) setSelectedDetail(basic);
        })
        .finally(() => {
          if (!cancelled) setDetailLoading(false);
        });
      return () => {
        cancelled = true;
      };
    }

    knowledgeApi
      .getEntity(activeLibrary.id, selectedNode)
      .then((rawDetail) => {
        if (cancelled) return;
        setSelectedDetail(mapKnowledgeEntityDetail(rawDetail, basic, selectedNode));
      })
      .catch((err: unknown) => {
        console.error('Entity detail failed:', err);
        toast.error(errorMessage(err, 'Failed to load entity details'));
      })
      .finally(() => {
        if (!cancelled) setDetailLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [activeLibrary, selectedNode, adjacency]);

  // Canonical "hide this node" set, recomputed only when filter inputs
  // change. SigmaGraph reads this and applies the hide flag via its
  // reducer pipeline — the Graphology instance is never rebuilt, so
  // typing in the search box does not trigger the multi-second layout
  // pass that a filtered-nodes-prop approach would cost at 100k nodes.
  const hiddenIds = useMemo(() => {
    const hidden = new Set<string>();
    const query = debouncedSearchQuery.trim().toLowerCase();
    const hasTypeFilter = hiddenTypes.size > 0;
    const hasSubTypeFilter = hiddenSubTypes.size > 0;
    const hasQuery = query.length > 0;
    if (!hasTypeFilter && !hasSubTypeFilter && !hasQuery) return hidden;
    for (const n of allNodes) {
      if (hasTypeFilter && hiddenTypes.has(n.type)) {
        hidden.add(n.id);
        continue;
      }
      if (hasSubTypeFilter && hiddenSubTypes.has(subtypeFilterKey(n.type, n.subType))) {
        hidden.add(n.id);
        continue;
      }
      if (hasQuery && !n.label.toLowerCase().includes(query)) {
        hidden.add(n.id);
      }
    }
    return hidden;
  }, [allNodes, hiddenTypes, hiddenSubTypes, debouncedSearchQuery]);

  const visibleNodeCount = allNodes.length - hiddenIds.size;

  const activeLayoutOption =
    GRAPH_LAYOUT_OPTIONS.find((option) => option.id === layout) ?? GRAPH_LAYOUT_OPTIONS[0];
  const recommendedLayout =
    graphMeta?.recommendedLayout && isGraphLayoutType(graphMeta.recommendedLayout)
      ? graphMeta.recommendedLayout
      : null;

  const typeLegend = useMemo(() => buildTypeLegend(allNodes), [allNodes]);

  const selected = selectedDetail ?? adjacency.nodeById.get(selectedNode ?? '') ?? null;

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">{t('graph.title')}</h1>
        </div>
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
      <div
        className="px-4 py-2.5 border-b flex items-center gap-2 flex-wrap"
        style={{
          background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--background)))',
        }}
      >
        <div className="relative min-w-[180px]">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input
            className="h-8 pl-8 text-xs"
            placeholder={t('graph.searchPlaceholder')}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>

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
            <span className="font-semibold text-primary">
              {t(`graph.layouts.${recommendedLayout}`)}
            </span>
          </button>
        )}

        {hasActiveGraphState && (
          <button
            className="h-7 px-2.5 text-xs flex items-center gap-1.5 rounded-lg hover:bg-muted transition-all duration-200 font-semibold"
            onClick={resetGraphView}
          >
            <X className="h-3.5 w-3.5" /> {t('graph.clear')}
          </button>
        )}

        <div className="ml-auto flex items-center gap-3 text-xs text-muted-foreground">
          <span className="tabular-nums font-semibold">
            {graphMeta?.nodeCount ?? 0} {t('graph.nodes')}
          </span>
          <span className="tabular-nums font-semibold">
            {graphMeta?.edgeCount ?? 0} {t('graph.edges')}
          </span>
          {(graphMeta?.hiddenDisconnectedCount ?? 0) > 0 && (
            <span className="tabular-nums">
              {graphMeta!.hiddenDisconnectedCount} {t('graph.hidden')}
            </span>
          )}
          <span
            className={`status-badge ${
              graphStatus === 'ready'
                ? 'status-ready'
                : graphStatus === 'partial'
                  ? 'status-warning'
                  : graphStatus === 'failed'
                    ? 'status-failed'
                    : 'status-processing'
            }`}
          >
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
              {loadProgress && loadProgress.total > 0 && (
                <p className="text-xs text-muted-foreground mt-1 tabular-nums">
                  {loadProgress.nodes} / {loadProgress.total}
                </p>
              )}
            </div>
          ) : loadError ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <AlertTriangle className="h-7 w-7 text-status-failed" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{t('graph.failedToLoad')}</h2>
              <p className="text-sm text-muted-foreground mt-2 max-w-sm text-center">{loadError}</p>
            </div>
          ) : visibleNodeCount === 0 ? (
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
                <Button
                  variant="outline"
                  size="sm"
                  className="mt-4"
                  onClick={() => navigate('/documents')}
                >
                  <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('graph.goToDocuments')}
                </Button>
              )}
            </div>
          ) : (
            <Suspense
              fallback={
                <div className="flex-1 flex items-center justify-center">
                  <Loader2 className="h-6 w-6 animate-spin" />
                </div>
              }
            >
              <SigmaGraph
                nodes={allNodes}
                edges={allEdges}
                selectedId={selectedNode}
                onSelect={handleSelectNode}
                layout={layout}
                hiddenIds={hiddenIds}
              />
            </Suspense>
          )}

          <GraphLegend
            t={t}
            legend={typeLegend}
            legendOpen={legendOpen}
            setLegendOpen={setLegendOpen}
            hiddenTypes={hiddenTypes}
            setHiddenTypes={(updater) => setHiddenTypes((prev) => updater(prev))}
            hiddenSubTypes={hiddenSubTypes}
            setHiddenSubTypes={(updater) => setHiddenSubTypes((prev) => updater(prev))}
            expandedSubtypeGroups={expandedSubtypeGroups}
            setExpandedSubtypeGroups={(updater) =>
              setExpandedSubtypeGroups((prev) => updater(prev))
            }
          />
        </div>

        {selected && (
          <GraphInspector
            t={t}
            selected={selected}
            detailLoading={detailLoading}
            adjacency={adjacency}
            onClose={() => handleSelectNode(null)}
            onSelectNode={handleSelectNode}
          />
        )}
      </div>
    </div>
  );
}
