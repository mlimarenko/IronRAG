import {
  Component,
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { useTranslation } from 'react-i18next';
import { useSuspenseQuery } from '@tanstack/react-query';
import i18n from '@/shared/i18n';
import { useApp } from '@/shared/contexts/app-context';
import { useNavigate, useSearchParams } from 'react-router-dom';
import {
  mapGraphDocumentDetail,
  mapGraphTopology,
  mapKnowledgeEntityDetail,
} from '@/features/graph/model/graphAdapter';
import { errorMessage } from '@/shared/lib/errorMessage';
import { knowledgeApi, queries } from '@/shared/api';
import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/shared/components/ui/tooltip';
import {
  GRAPH_LAYOUT_OPTIONS,
  GRAPH_EDGE_DENSITY_TOGGLE_MAX_EDGES,
  GRAPH_EDGE_RENDER_CAP,
  DEFAULT_GRAPH_LAYOUT,
  normalizeRecommendedGraphLayout,
  type GraphLayoutType,
} from '@/features/graph/model/config';
import {
  Search,
  Loader2,
  FileText,
  Share2,
  AlertTriangle,
  PieChart,
  Rows3,
  Network,
  CircleDashed,
  Orbit,
  Maximize2,
} from 'lucide-react';
import type { GraphEdge, GraphMetadata, GraphNode, GraphStatus } from '@/shared/types';
import { GraphInspector } from '@/features/graph/components/GraphInspector';
import { GraphLegend } from '@/features/graph/components/GraphLegend';
import { buildTypeLegend, subtypeFilterKey } from '@/features/graph/model/typeLegend';
import { useGraphAdjacency, type GraphAdjacencyIndex } from '@/features/graph/hooks/useGraphAdjacency';

const SigmaGraph = lazy(() => import('@/features/graph/components/SigmaGraph'));

const GRAPH_LAYOUT_ICONS = {
  sectors: PieChart,
  bands: Rows3,
  components: Network,
  rings: CircleDashed,
  clusters: Orbit,
  hubs: Network,
  sources: FileText,
  flow: Share2,
  radial: CircleDashed,
  circlepack: Orbit,
} as const;

type GraphInspectorDetailProps = {
  t: ReturnType<typeof useTranslation>['t'];
  activeLibraryId: string;
  selectedBasic: GraphNode;
  adjacency: GraphAdjacencyIndex;
  onClose: () => void;
  onSelectNode: (id: string) => void;
  onFocusNeighborhood: (id: string) => void;
};

function GraphInspectorLoadingFallback({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Omit<GraphInspectorDetailProps, 'activeLibraryId'>) {
  return (
    <GraphInspector
      t={t}
      selected={selectedBasic}
      detailLoading
      adjacency={adjacency}
      onClose={onClose}
      onSelectNode={onSelectNode}
      onFocusNeighborhood={onFocusNeighborhood}
    />
  );
}

function GraphInspectorDetailErrorFallback({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Omit<GraphInspectorDetailProps, 'activeLibraryId'>) {
  return (
    <GraphInspector
      t={t}
      selected={selectedBasic}
      detailLoading={false}
      detailError={t('graph.detailsFailed')}
      adjacency={adjacency}
      onClose={onClose}
      onSelectNode={onSelectNode}
      onFocusNeighborhood={onFocusNeighborhood}
    />
  );
}

function DocumentGraphInspector({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Omit<GraphInspectorDetailProps, 'activeLibraryId'>) {
  const detailQuery = useSuspenseQuery({
    ...queries.getContentDocumentOptions({
      path: { documentId: selectedBasic.id },
    }),
    select: (doc) => mapGraphDocumentDetail(doc, selectedBasic, selectedBasic.id),
    retry: false,
  });

  return (
    <GraphInspector
      t={t}
      selected={detailQuery.data}
      detailLoading={false}
      adjacency={adjacency}
      onClose={onClose}
      onSelectNode={onSelectNode}
      onFocusNeighborhood={onFocusNeighborhood}
    />
  );
}

function EntityGraphInspector({
  t,
  activeLibraryId,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: GraphInspectorDetailProps) {
  const detailQuery = useSuspenseQuery({
    ...queries.getKnowledgeEntityOptions({
      path: { libraryId: activeLibraryId, entityId: selectedBasic.id },
    }),
    select: (rawDetail) =>
      mapKnowledgeEntityDetail(rawDetail, selectedBasic, selectedBasic.id),
  });

  return (
    <GraphInspector
      t={t}
      selected={detailQuery.data}
      detailLoading={false}
      adjacency={adjacency}
      onClose={onClose}
      onSelectNode={onSelectNode}
      onFocusNeighborhood={onFocusNeighborhood}
    />
  );
}

function SuspendedGraphInspector(props: GraphInspectorDetailProps) {
  return props.selectedBasic.type === 'document' ? (
    <DocumentGraphInspector {...props} />
  ) : (
    <EntityGraphInspector {...props} />
  );
}

class GraphInspectorErrorBoundary extends Component<
  { children: ReactNode; fallback: ReactNode; resetKey: string },
  { error: unknown }
> {
  state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  componentDidCatch(error: unknown) {
    void import('@/shared/lib/observability').then(({ captureUiException }) =>
      captureUiException(error, { feature: 'graph-inspector-detail' }),
    );
  }

  componentDidUpdate(prevProps: { resetKey: string }) {
    if (prevProps.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null });
    }
  }

  render() {
    return this.state.error ? this.props.fallback : this.props.children;
  }
}

export default function GraphPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const requestedNodeId = searchParams.get('nodeId');

  // Graph data
  const [allNodes, setAllNodes] = useState<GraphNode[]>([]);
  const [allEdges, setAllEdges] = useState<GraphEdge[]>([]);
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null);
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building');
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadProgress, setLoadProgress] = useState<{ nodes: number; total: number } | null>(null);

  // Selection state
  const [selectedNode, setSelectedNode] = useState<string | null>(null);

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
  const [layout, setLayout] = useState<GraphLayoutType>(DEFAULT_GRAPH_LAYOUT);
  // When the graph has more edges than the render cap, only a sampled subset
  // is drawn for smoothness; this toggle raises the sample density without
  // asking Firefox to paint the entire edge set every frame.
  const [showDenseEdges, setShowDenseEdges] = useState(false);
  const [legendOpen, setLegendOpen] = useState(true);
  const [expandedSubtypeGroups, setExpandedSubtypeGroups] = useState<Set<string>>(new Set());
  const [focusedNeighborhoodId, setFocusedNeighborhoodId] = useState<string | null>(null);

  useEffect(() => {
    const handle = setTimeout(() => setDebouncedSearchQuery(searchQuery), 250);
    return () => clearTimeout(handle);
  }, [searchQuery]);

  // Canonical adjacency index — computed once per (nodes, edges). The inspector,
  // search, and any future neighbor lookup read from this in O(k) per query
  // instead of scanning every edge on each render.
  const adjacency = useGraphAdjacency(allNodes, allEdges);
  const activeSelectedNode =
    selectedNode && adjacency.nodeById.has(selectedNode) ? selectedNode : null;

  const handleSelectNode = useCallback((nextId: string | null) => {
    setSelectedNode(nextId);
    if (typeof window === 'undefined') return;

    const nextParams = new URLSearchParams(window.location.search);
    if (nextId) {
      nextParams.set('nodeId', nextId);
    } else {
      nextParams.delete('nodeId');
    }
    const search = nextParams.toString();
    const nextUrl = `${window.location.pathname}${search ? `?${search}` : ''}${window.location.hash}`;
    window.history.replaceState(window.history.state, '', nextUrl);
  }, []);

  useEffect(() => {
    if (!requestedNodeId || !adjacency.nodeById.has(requestedNodeId)) return;
    const handle = window.setTimeout(() => {
      setSelectedNode((current) => (current === requestedNodeId ? current : requestedNodeId));
    }, 0);
    return () => window.clearTimeout(handle);
  }, [requestedNodeId, adjacency.nodeById]);

  const fitViewRef = useRef<(() => void) | null>(null);
  const handleFitViewReady = useCallback((fn: () => void) => {
    fitViewRef.current = fn;
  }, []);
  const updateHiddenTypes = useCallback((updater: (prev: Set<string>) => Set<string>) => {
    setHiddenTypes((prev) => updater(prev));
  }, []);
  const updateHiddenSubTypes = useCallback((updater: (prev: Set<string>) => Set<string>) => {
    setHiddenSubTypes((prev) => updater(prev));
  }, []);
  const updateExpandedSubtypeGroups = useCallback(
    (updater: (prev: Set<string>) => Set<string>) => {
      setExpandedSubtypeGroups((prev) => updater(prev));
    },
    [],
  );
  const closeInspector = useCallback(() => {
    handleSelectNode(null);
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
      setSearchQuery('');
      setHiddenTypes(new Set());
      setHiddenSubTypes(new Set());
      setFocusedNeighborhoodId(null);

      try {
        // Streaming topology with onProgress callback — TanStack Query's queryFn
        // doesn't model incremental progress, so the imperative shim stays
        // canonical for this path. All other GraphPage server-state reads
        // flow through TanStack Query.
        // eslint-disable-next-line no-restricted-syntax -- streaming progress fetch, see comment above
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
          normalizeRecommendedGraphLayout(topologyMeta.recommendedLayout) ?? DEFAULT_GRAPH_LAYOUT;

        setAllNodes(topologyNodes);
        setAllEdges(topologyEdges);
        setGraphMeta(topologyMeta);
        setGraphStatus(topologyMeta.status);
        setLayout(recommendedLayout);
        setLoadProgress(null);
      } catch (err: unknown) {
        if (cancelled) return;
        setLoadError(errorMessage(err, i18n.t('graph.failedToLoad')));
        setGraphStatus('failed');
        setLoadProgress(null);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [activeLibrary]);

  // Look up the basic node for the current selection from the adjacency index.
  // Used to gate the detail queries and as a fallback when detail is unavailable.
  const selectedBasic = activeSelectedNode
    ? (adjacency.nodeById.get(activeSelectedNode) ?? null)
    : null;
  // Dense graphs use a local canvas overlay inside SigmaGraph for selected
  // neighborhoods, so selection still reaches the graph without forcing the
  // expensive whole-canvas reducer path.
  const sigmaSelectedNode = activeSelectedNode;
  const focusedNeighborhoodIds = useMemo(() => {
    if (!focusedNeighborhoodId || !adjacency.nodeById.has(focusedNeighborhoodId)) return null;
    const visible = new Set<string>([focusedNeighborhoodId]);
    const neighbors = adjacency.neighborIds.get(focusedNeighborhoodId) ?? [];
    for (const id of neighbors) visible.add(id);
    return visible;
  }, [adjacency.neighborIds, adjacency.nodeById, focusedNeighborhoodId]);
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
        continue;
      }
      if (focusedNeighborhoodIds && !focusedNeighborhoodIds.has(n.id)) {
        hidden.add(n.id);
      }
    }
    return hidden;
  }, [allNodes, hiddenTypes, hiddenSubTypes, debouncedSearchQuery, focusedNeighborhoodIds]);

  const visibleNodeCount = allNodes.length - hiddenIds.size;

  const activeLayoutOption =
    GRAPH_LAYOUT_OPTIONS.find((option) => option.id === layout) ?? GRAPH_LAYOUT_OPTIONS[0];
  const recommendedLayout = normalizeRecommendedGraphLayout(graphMeta?.recommendedLayout);

  const typeLegend = useMemo(() => buildTypeLegend(allNodes), [allNodes]);

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
        role="toolbar"
        aria-label={t('graph.toolbar')}
        className="min-h-[3rem] overflow-x-auto overflow-y-hidden border-b px-4 py-1.5 flex items-center gap-2 whitespace-nowrap"
        style={{
          background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--background)))',
        }}
      >
        <div className="relative w-[13rem] shrink-0">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input
            className="h-8 pl-8 text-xs"
            placeholder={t('graph.searchPlaceholder')}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>

        <TooltipProvider delayDuration={180}>
          <div className="flex shrink-0 items-center gap-2">
            <div className="flex items-center gap-1 rounded-xl border border-border/60 bg-card/80 p-0.5 shadow-soft">
              {GRAPH_LAYOUT_OPTIONS.map((option) => {
                const isActive = layout === option.id;
                const Icon = GRAPH_LAYOUT_ICONS[option.iconKey];
                return (
                  <Tooltip key={option.id}>
                    <TooltipTrigger asChild>
                      <button
                        type="button"
                        onClick={() => setLayout(option.id)}
                        className={`flex h-7 w-7 items-center justify-center rounded-lg transition-all ${
                          isActive
                            ? 'bg-primary text-primary-foreground shadow-sm'
                            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                        }`}
                        aria-label={t(option.labelKey)}
                        aria-pressed={isActive}
                      >
                        <Icon className="h-4 w-4" />
                      </button>
                    </TooltipTrigger>
                    <TooltipContent side="bottom" align="center" className="max-w-64">
                      <div className="text-xs font-semibold">{t(option.labelKey)}</div>
                      <div className="mt-1 text-xs leading-snug text-muted-foreground">
                        {t(option.descriptionKey)}
                      </div>
                    </TooltipContent>
                  </Tooltip>
                );
              })}
            </div>
            <Tooltip>
              <TooltipTrigger asChild>
                <div className="flex min-w-0 max-w-[120px] cursor-help items-center xl:max-w-[180px]">
                  <span className="truncate text-xs font-semibold text-foreground">
                    {t(activeLayoutOption.labelKey)}
                  </span>
                </div>
              </TooltipTrigger>
              <TooltipContent side="bottom" align="start" className="max-w-64">
                <div className="text-xs leading-snug">{t(activeLayoutOption.descriptionKey)}</div>
              </TooltipContent>
            </Tooltip>
          </div>
        </TooltipProvider>

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

        {/* Fit-to-view button — always visible when graph has nodes */}
        {allNodes.length > 0 && (
          <button
            type="button"
            aria-label={t('graph.zoomToFit')}
            title={t('graph.zoomToFit')}
            className="h-7 w-7 flex items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-all duration-200"
            onClick={() => fitViewRef.current?.()}
          >
            <Maximize2 className="h-3.5 w-3.5" />
          </button>
        )}

        {/* Show-all-edges toggle — only when the edge count is capped */}
        {(graphMeta?.edgeCount ?? 0) > GRAPH_EDGE_RENDER_CAP &&
          (graphMeta?.edgeCount ?? 0) <= GRAPH_EDGE_DENSITY_TOGGLE_MAX_EDGES && (
          <button
            type="button"
            data-perf-id="edge-density-toggle"
            aria-label={showDenseEdges ? t('graph.showSampledEdges') : t('graph.showDenseEdges')}
            aria-pressed={showDenseEdges}
            title={showDenseEdges ? t('graph.showSampledEdgesHint') : t('graph.showDenseEdgesHint')}
            className={`h-7 w-7 flex items-center justify-center rounded-lg transition-all duration-200 ${
              showDenseEdges
                ? 'bg-primary/15 text-primary'
                : 'text-muted-foreground hover:bg-muted hover:text-foreground'
            }`}
            onClick={() => setShowDenseEdges((v) => !v)}
          >
            <Share2 className="h-3.5 w-3.5" />
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
            {t(`graph.statusLabels.${graphStatus}`)}
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
                selectedId={sigmaSelectedNode}
                onSelect={handleSelectNode}
                layout={layout}
                hiddenIds={hiddenIds}
                showDenseEdges={showDenseEdges}
                onFitViewReady={handleFitViewReady}
              />
            </Suspense>
          )}

          <GraphLegend
            t={t}
            legend={typeLegend}
            legendOpen={legendOpen}
            setLegendOpen={setLegendOpen}
            hiddenTypes={hiddenTypes}
            setHiddenTypes={updateHiddenTypes}
            hiddenSubTypes={hiddenSubTypes}
            setHiddenSubTypes={updateHiddenSubTypes}
            expandedSubtypeGroups={expandedSubtypeGroups}
            setExpandedSubtypeGroups={updateExpandedSubtypeGroups}
          />
        </div>

        {selectedBasic && (
          <GraphInspectorErrorBoundary
            resetKey={selectedBasic.id}
            fallback={
              <GraphInspectorDetailErrorFallback
                t={t}
                selectedBasic={selectedBasic}
                adjacency={adjacency}
                onClose={closeInspector}
                onSelectNode={handleSelectNode}
                onFocusNeighborhood={setFocusedNeighborhoodId}
              />
            }
          >
            <Suspense
              fallback={
                <GraphInspectorLoadingFallback
                  t={t}
                  selectedBasic={selectedBasic}
                  adjacency={adjacency}
                  onClose={closeInspector}
                  onSelectNode={handleSelectNode}
                  onFocusNeighborhood={setFocusedNeighborhoodId}
                />
              }
            >
              <SuspendedGraphInspector
                t={t}
                activeLibraryId={activeLibrary.id}
                selectedBasic={selectedBasic}
                adjacency={adjacency}
                onClose={closeInspector}
                onSelectNode={handleSelectNode}
                onFocusNeighborhood={setFocusedNeighborhoodId}
              />
            </Suspense>
          </GraphInspectorErrorBoundary>
        )}
      </div>
    </div>
  );
}
