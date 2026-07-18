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
} from 'react'
import { useTranslation } from 'react-i18next'
import { useSuspenseQuery } from '@tanstack/react-query'
import i18n from '@/shared/i18n'
import { useApp } from '@/shared/contexts/app-context'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  mapGraphDocumentDetail,
  mapGraphTopology,
  mapKnowledgeEntityDetail,
} from '@/features/graph/model/graphAdapter'
import { errorMessage } from '@/shared/lib/errorMessage'
import { knowledgeApi, queries } from '@/shared/api'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import {
  GRAPH_EDGE_DENSITY_TOGGLE_MAX_EDGES,
  GRAPH_EDGE_RENDER_CAP,
  DEFAULT_GRAPH_LAYOUT,
  normalizeRecommendedGraphLayout,
  type GraphLayoutType,
} from '@/features/graph/model/config'
import { Search, Loader2, FileText, Share2, AlertTriangle, Maximize2, Sparkles } from 'lucide-react'
import type { GraphEdge, GraphMetadata, GraphNode, GraphStatus } from '@/shared/types'
import { GraphInspector } from '@/features/graph/components/GraphInspector'
import { GraphLayoutPicker } from '@/features/graph/components/GraphLayoutPicker'
import { GraphLegend } from '@/features/graph/components/GraphLegend'
import { StatusBadge } from '@/shared/components/StatusBadge'
import { buildTypeLegend, subtypeFilterKey } from '@/features/graph/model/typeLegend'
import {
  useGraphAdjacency,
  type GraphAdjacencyIndex,
} from '@/features/graph/hooks/useGraphAdjacency'

const SigmaGraph = lazy(() => import('@/features/graph/components/SigmaGraph'))

function defaultLegendOpen(): boolean {
  if (typeof window === 'undefined') return true
  return window.matchMedia('(min-width: 768px)').matches
}

type GraphInspectorDetailProps = Readonly<{
  t: ReturnType<typeof useTranslation>['t']
  activeLibraryId: string
  selectedBasic: GraphNode
  adjacency: GraphAdjacencyIndex
  onClose: () => void
  onSelectNode: (id: string) => void
  onFocusNeighborhood: (id: string) => void
}>

function GraphInspectorLoadingFallback({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Readonly<Omit<GraphInspectorDetailProps, 'activeLibraryId'>>) {
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
  )
}

function GraphInspectorDetailErrorFallback({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Readonly<Omit<GraphInspectorDetailProps, 'activeLibraryId'>>) {
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
  )
}

function DocumentGraphInspector({
  t,
  selectedBasic,
  adjacency,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: Readonly<Omit<GraphInspectorDetailProps, 'activeLibraryId'>>) {
  const detailQuery = useSuspenseQuery({
    ...queries.getContentDocumentOptions({
      path: { documentId: selectedBasic.id },
    }),
    select: (doc) => mapGraphDocumentDetail(doc, selectedBasic, selectedBasic.id),
    retry: false,
  })

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
  )
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
    select: (rawDetail) => mapKnowledgeEntityDetail(rawDetail, selectedBasic, selectedBasic.id),
  })

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
  )
}

function SuspendedGraphInspector(props: Readonly<GraphInspectorDetailProps>) {
  return props.selectedBasic.type === 'document' ? (
    <DocumentGraphInspector {...props} />
  ) : (
    <EntityGraphInspector {...props} />
  )
}

class GraphInspectorErrorBoundary extends Component<
  { children: ReactNode; fallback: ReactNode; resetKey: string },
  { error: unknown }
> {
  state: { error: unknown } = { error: null }

  static getDerivedStateFromError(error: unknown) {
    return { error }
  }

  componentDidCatch(error: unknown) {
    void import('@/shared/lib/observability').then(({ captureUiException }) =>
      captureUiException(error, { feature: 'graph-inspector-detail' }),
    )
  }

  componentDidUpdate(prevProps: { resetKey: string }) {
    if (prevProps.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null })
    }
  }

  render() {
    return this.state.error ? this.props.fallback : this.props.children
  }
}

function graphStatusTone(status: GraphStatus): 'ready' | 'warning' | 'failed' | 'processing' {
  if (status === 'ready') return 'ready'
  if (status === 'partial') return 'warning'
  if (status === 'failed') return 'failed'
  return 'processing'
}

function graphEmptyStateContent(
  hasNodes: boolean,
  t: ReturnType<typeof useTranslation>['t'],
  navigate: ReturnType<typeof useNavigate>,
) {
  return {
    title: hasNodes ? t('graph.noMatchingNodes') : t('graph.noGraph'),
    description: hasNodes ? t('graph.noMatchingNodesDesc') : t('graph.noGraphDesc'),
    action: hasNodes ? undefined : (
      <Button variant="outline" size="sm" onClick={() => navigate('/documents')}>
        <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('graph.goToDocuments')}
      </Button>
    ),
  }
}

function GraphCanvas({
  graphStatus,
  loadError,
  loadProgress,
  visibleNodeCount,
  emptyGraphContent,
  t,
  allNodes,
  allEdges,
  sigmaSelectedNode,
  handleSelectNode,
  layout,
  hiddenIds,
  showDenseEdges,
  handleFitViewReady,
}: Readonly<{
  graphStatus: GraphStatus
  loadError: string | null
  loadProgress: { nodes: number; total: number } | null
  visibleNodeCount: number
  emptyGraphContent: { title: string; description: string; action: ReactNode }
  t: ReturnType<typeof useTranslation>['t']
  allNodes: GraphNode[]
  allEdges: GraphEdge[]
  sigmaSelectedNode: string | null
  handleSelectNode: (id: string | null) => void
  layout: GraphLayoutType
  hiddenIds: Set<string>
  showDenseEdges: boolean
  handleFitViewReady: (fit: () => void) => void
}>) {
  if (graphStatus === 'building' || graphStatus === 'rebuilding') {
    return (
      <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
        <Loader2 className="h-8 w-8 animate-spin text-primary/60 mb-3" />
        <p className="text-sm font-semibold text-muted-foreground">{t('graph.loading')}</p>
        {loadProgress && loadProgress.total > 0 && (
          <p className="text-xs text-muted-foreground mt-1 tabular-nums">
            {loadProgress.nodes} / {loadProgress.total}
          </p>
        )}
      </div>
    )
  }
  if (loadError) {
    return (
      <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
        <WorkbenchEmptyState
          icon={<AlertTriangle className="h-7 w-7 text-status-failed" />}
          title={t('graph.failedToLoad')}
          description={loadError}
        />
      </div>
    )
  }
  if (visibleNodeCount === 0) {
    return (
      <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
        <WorkbenchEmptyState
          icon={<Share2 className="h-7 w-7 text-muted-foreground" />}
          title={emptyGraphContent.title}
          description={emptyGraphContent.description}
          action={emptyGraphContent.action}
        />
      </div>
    )
  }
  return (
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
  )
}

export default function GraphPage() {
  const { t } = useTranslation()
  const { activeLibrary } = useApp()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const requestedNodeId = searchParams.get('nodeId')

  // Graph data
  const [allNodes, setAllNodes] = useState<GraphNode[]>([])
  const [allEdges, setAllEdges] = useState<GraphEdge[]>([])
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null)
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building')
  const [loadError, setLoadError] = useState<string | null>(null)
  const [loadProgress, setLoadProgress] = useState<{ nodes: number; total: number } | null>(null)

  // Selection state
  const [selectedNode, setSelectedNode] = useState<string | null>(null)

  // UI controls
  const [searchQuery, setSearchQuery] = useState('')
  // `debouncedSearchQuery` trails `searchQuery` by 250 ms so rapid typing
  // does not recompute `hiddenIds` on every keystroke. On a 100k-node graph
  // the filter scan is still fast, but the Sigma reducer refresh that
  // follows each change is not — debouncing keeps the UI glassy while the
  // user is still composing their query.
  const [debouncedSearchQuery, setDebouncedSearchQuery] = useState('')
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set())
  const [hiddenSubTypes, setHiddenSubTypes] = useState<Set<string>>(new Set())
  const [layout, setLayout] = useState<GraphLayoutType>(DEFAULT_GRAPH_LAYOUT)
  // When the graph has more edges than the render cap, only a sampled subset
  // is drawn for smoothness; this toggle raises the sample density without
  // asking Firefox to paint the entire edge set every frame.
  const [showDenseEdges, setShowDenseEdges] = useState(false)
  const [legendOpen, setLegendOpen] = useState(defaultLegendOpen)
  const [expandedSubtypeGroups, setExpandedSubtypeGroups] = useState<Set<string>>(new Set())
  const [focusedNeighborhoodId, setFocusedNeighborhoodId] = useState<string | null>(null)

  useEffect(() => {
    const handle = setTimeout(() => setDebouncedSearchQuery(searchQuery), 250)
    return () => clearTimeout(handle)
  }, [searchQuery])

  // Canonical adjacency index — computed once per (nodes, edges). The inspector,
  // search, and any future neighbor lookup read from this in O(k) per query
  // instead of scanning every edge on each render.
  const adjacency = useGraphAdjacency(allNodes, allEdges)
  const activeSelectedNode =
    selectedNode && adjacency.nodeById.has(selectedNode) ? selectedNode : null

  const handleSelectNode = useCallback((nextId: string | null) => {
    setSelectedNode(nextId)
    if (typeof window === 'undefined') return

    const nextParams = new URLSearchParams(window.location.search)
    if (nextId) {
      nextParams.set('nodeId', nextId)
    } else {
      nextParams.delete('nodeId')
    }
    const search = nextParams.toString()
    const searchSuffix = search ? `?${search}` : ''
    const nextUrl = `${window.location.pathname}${searchSuffix}${window.location.hash}`
    window.history.replaceState(window.history.state, '', nextUrl)
  }, [])

  useEffect(() => {
    if (!requestedNodeId || !adjacency.nodeById.has(requestedNodeId)) return
    const handle = window.setTimeout(() => {
      setSelectedNode((current) => (current === requestedNodeId ? current : requestedNodeId))
    }, 0)
    return () => window.clearTimeout(handle)
  }, [requestedNodeId, adjacency.nodeById])

  const fitViewRef = useRef<(() => void) | null>(null)
  const handleFitViewReady = useCallback((fn: () => void) => {
    fitViewRef.current = fn
  }, [])
  const updateHiddenTypes = useCallback((updater: (prev: Set<string>) => Set<string>) => {
    setHiddenTypes((prev) => updater(prev))
  }, [])
  const updateHiddenSubTypes = useCallback((updater: (prev: Set<string>) => Set<string>) => {
    setHiddenSubTypes((prev) => updater(prev))
  }, [])
  const updateExpandedSubtypeGroups = useCallback((updater: (prev: Set<string>) => Set<string>) => {
    setExpandedSubtypeGroups((prev) => updater(prev))
  }, [])
  const closeInspector = useCallback(() => {
    handleSelectNode(null)
  }, [handleSelectNode])

  // Fetch graph topology on library change. Uses cancellation guard so a
  // rapid library switch does not commit stale data.
  useEffect(() => {
    if (!activeLibrary) return
    let cancelled = false

    void (async () => {
      setGraphStatus('building')
      setLoadError(null)
      setAllNodes([])
      setAllEdges([])
      setGraphMeta(null)
      setLoadProgress({ nodes: 0, total: 0 })
      setSelectedNode(null)
      setSearchQuery('')
      setHiddenTypes(new Set())
      setHiddenSubTypes(new Set())
      setFocusedNeighborhoodId(null)

      try {
        // Streaming topology with onProgress callback — TanStack Query's queryFn
        // doesn't model incremental progress, so the imperative shim stays
        // canonical for this path. All other GraphPage server-state reads
        // flow through TanStack Query.
        // eslint-disable-next-line no-restricted-syntax -- streaming progress fetch, see comment above
        const topologyRes = await knowledgeApi.getGraphTopology(activeLibrary.id, {
          onProgress: (progress) => {
            if (cancelled) return
            setLoadProgress({
              nodes: progress.nodesLoaded,
              total: progress.expectedNodes,
            })
          },
        })
        if (cancelled) return
        const {
          nodes: topologyNodes,
          edges: topologyEdges,
          meta: topologyMeta,
        } = mapGraphTopology(topologyRes)
        const recommendedLayout =
          normalizeRecommendedGraphLayout(topologyMeta.recommendedLayout) ?? DEFAULT_GRAPH_LAYOUT

        setAllNodes(topologyNodes)
        setAllEdges(topologyEdges)
        setGraphMeta(topologyMeta)
        setGraphStatus(topologyMeta.status)
        setLayout(recommendedLayout)
        setLoadProgress(null)
      } catch (err: unknown) {
        if (cancelled) return
        setLoadError(errorMessage(err, i18n.t('graph.failedToLoad')))
        setGraphStatus('failed')
        setLoadProgress(null)
      }
    })()

    return () => {
      cancelled = true
    }
  }, [activeLibrary])

  // Look up the basic node for the current selection from the adjacency index.
  // Used to gate the detail queries and as a fallback when detail is unavailable.
  const selectedBasic = activeSelectedNode
    ? (adjacency.nodeById.get(activeSelectedNode) ?? null)
    : null
  // Dense graphs use a local canvas overlay inside SigmaGraph for selected
  // neighborhoods, so selection still reaches the graph without forcing the
  // expensive whole-canvas reducer path.
  const sigmaSelectedNode = activeSelectedNode
  const focusedNeighborhoodIds = useMemo(() => {
    if (!focusedNeighborhoodId || !adjacency.nodeById.has(focusedNeighborhoodId)) return null
    const visible = new Set<string>([focusedNeighborhoodId])
    const neighbors = adjacency.neighborIds.get(focusedNeighborhoodId) ?? []
    for (const id of neighbors) visible.add(id)
    return visible
  }, [adjacency.neighborIds, adjacency.nodeById, focusedNeighborhoodId])
  // Canonical "hide this node" set, recomputed only when filter inputs
  // change. SigmaGraph reads this and applies the hide flag via its
  // reducer pipeline — the Graphology instance is never rebuilt, so
  // typing in the search box does not trigger the multi-second layout
  // pass that a filtered-nodes-prop approach would cost at 100k nodes.
  const hiddenIds = useMemo(() => {
    const hidden = new Set<string>()
    const query = debouncedSearchQuery.trim().toLowerCase()
    const hasTypeFilter = hiddenTypes.size > 0
    const hasSubTypeFilter = hiddenSubTypes.size > 0
    const hasQuery = query.length > 0
    if (!hasTypeFilter && !hasSubTypeFilter && !hasQuery) return hidden
    for (const n of allNodes) {
      if (hasTypeFilter && hiddenTypes.has(n.type)) {
        hidden.add(n.id)
        continue
      }
      if (hasSubTypeFilter && hiddenSubTypes.has(subtypeFilterKey(n.type, n.subType))) {
        hidden.add(n.id)
        continue
      }
      if (hasQuery && !n.label.toLowerCase().includes(query)) {
        hidden.add(n.id)
        continue
      }
      if (focusedNeighborhoodIds && !focusedNeighborhoodIds.has(n.id)) {
        hidden.add(n.id)
      }
    }
    return hidden
  }, [allNodes, hiddenTypes, hiddenSubTypes, debouncedSearchQuery, focusedNeighborhoodIds])

  const visibleNodeCount = allNodes.length - hiddenIds.size

  const recommendedLayout = normalizeRecommendedGraphLayout(graphMeta?.recommendedLayout)
  const emptyGraphContent = graphEmptyStateContent(allNodes.length > 0, t, navigate)

  const typeLegend = useMemo(() => buildTypeLegend(allNodes), [allNodes])

  if (!activeLibrary) {
    return (
      <PageShell header={<PageHeader title={t('graph.title')} />} bodyClassName="empty-state">
        <WorkbenchEmptyState
          icon={<Share2 className="h-7 w-7 text-muted-foreground" />}
          title={t('graph.noLibrary')}
          description={t('graph.noLibraryDesc')}
        />
      </PageShell>
    )
  }

  return (
    <PageShell
      header={<PageHeader title={t('graph.title')} description={t('graph.subtitle')} />}
      bodyClassName="flex flex-col overflow-hidden p-3 sm:p-4"
    >
      {/* Toolbar */}
      <div
        role="toolbar"
        aria-label={t('graph.toolbar')}
        className="relative z-20 flex min-h-12 flex-col items-stretch gap-3 overflow-visible workbench-surface px-3 py-2 sm:flex-row sm:items-center"
      >
        <div className="relative w-full shrink-0 sm:w-[16rem]">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
          <Input
            className="h-9 rounded-lg pl-9 text-sm"
            placeholder={t('graph.searchPlaceholder')}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>

        <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
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
          <StatusBadge tone={graphStatusTone(graphStatus)}>
            {t(`graph.statusLabels.${graphStatus}`)}
          </StatusBadge>
        </div>

        <div aria-hidden="true" className="hidden h-8 w-px shrink-0 bg-border md:block" />

        <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto">
          <span className="hidden section-label lg:inline">{t('graph.layoutControls')}</span>
          <GraphLayoutPicker
            value={layout}
            recommended={recommendedLayout}
            onChange={setLayout}
            t={t}
          />
        </div>

        {recommendedLayout && layout !== recommendedLayout && (
          <button
            type="button"
            onClick={() => setLayout(recommendedLayout)}
            className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-lg border bg-card px-2.5 text-xs font-medium text-muted-foreground shadow-soft transition-colors hover:bg-muted/60"
          >
            <Sparkles className="h-3.5 w-3.5 text-primary" />
            <span>{t('graph.recommended')}</span>
            <span className="font-semibold text-primary">
              {t(`graph.layouts.${recommendedLayout}`)}
            </span>
          </button>
        )}

        <div className="flex w-full shrink-0 items-center justify-between gap-2 sm:ml-auto sm:w-auto sm:justify-start">
          <span className="hidden section-label lg:inline">{t('graph.viewControls')}</span>
          <span className="text-xs font-semibold tabular-nums text-muted-foreground">
            {visibleNodeCount} / {allNodes.length} {t('graph.nodes')}
          </span>

          {/* Fit-to-view button — always visible when graph has nodes */}
          {allNodes.length > 0 && (
            <button
              type="button"
              aria-label={t('graph.zoomToFit')}
              title={t('graph.zoomToFit')}
              className="flex h-9 w-9 items-center justify-center rounded-lg text-muted-foreground transition-all duration-200 hover:bg-muted hover:text-foreground"
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
                aria-label={
                  showDenseEdges ? t('graph.showSampledEdges') : t('graph.showDenseEdges')
                }
                aria-pressed={showDenseEdges}
                title={
                  showDenseEdges ? t('graph.showSampledEdgesHint') : t('graph.showDenseEdgesHint')
                }
                className={`h-7 w-7 flex items-center justify-center rounded-lg transition-all duration-200 ${
                  showDenseEdges
                    ? 'bg-primary text-primary-foreground shadow-soft'
                    : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                }`}
                onClick={() => setShowDenseEdges((v) => !v)}
              >
                <Share2 className="h-3.5 w-3.5" />
              </button>
            )}
        </div>
      </div>

      <div className="relative mt-3 min-h-0 flex-1 overflow-hidden workbench-surface">
        <div className="absolute inset-0">
          <GraphCanvas
            graphStatus={graphStatus}
            loadError={loadError}
            loadProgress={loadProgress}
            visibleNodeCount={visibleNodeCount}
            emptyGraphContent={emptyGraphContent}
            t={t}
            allNodes={allNodes}
            allEdges={allEdges}
            sigmaSelectedNode={sigmaSelectedNode}
            handleSelectNode={handleSelectNode}
            layout={layout}
            hiddenIds={hiddenIds}
            showDenseEdges={showDenseEdges}
            handleFitViewReady={handleFitViewReady}
          />

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
    </PageShell>
  )
}
