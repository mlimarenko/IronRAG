import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  FileText,
  Inbox,
  ListFilter,
  LoaderCircle,
  Maximize,
  Search,
  SlidersHorizontal,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { ChooseLibraryButton } from '@/app/ChooseLibraryButton'
import { useApp } from '@/shared/contexts/app-context'
import { knowledgeApi, queries } from '@/shared/api'
import { formatNumber } from '@/shared/format'
import { errorMessage } from '@/shared/lib/errorMessage'
import { graphStatusLabel } from '@/shared/lib/enumLabels'
import type { GraphEdge, GraphMetadata, GraphNode, GraphStatus } from '@/shared/types'
import { Badge, Button, Drawer, PageHeader, Progress } from '@/shared/ui'
import { GraphCanvas } from './GraphCanvas'
import { GraphInspector } from './components/GraphInspector'
import { GraphLayoutPicker } from './components/GraphLayoutPicker'
import { GraphLegend } from './components/GraphLegend'
import { useGraphAdjacency } from './hooks/useGraphAdjacency'
import { DEFAULT_GRAPH_LAYOUT, GRAPH_EDGE_RENDER_CAP, type GraphLayoutType } from './model/config'
import { mapGraphTopology } from './model/graphAdapter'
import { resolveGraphStatus, type StreamPhase } from './model/graphStatus'
import {
  EMPTY_LEGEND_FILTER,
  buildTypeLegend,
  isNodeHiddenByLegend,
  type LegendFilter,
} from './model/typeLegend'

type LoadProgress = { nodes: number; total: number }

function summaryStatus(summary: ReturnType<typeof useGraphSummary>['data']): GraphStatus | null {
  if (!summary) return null
  if (summary.nodeCount === 0) return 'empty'
  if (summary.graphSparseDocumentCount > 0) return 'partial'
  return 'ready'
}

function statusTone(status: GraphStatus): string {
  if (status === 'ready') return 'ready'
  if (status === 'partial' || status === 'stale') return 'partial'
  if (status === 'failed') return 'failed'
  if (status === 'building' || status === 'rebuilding') return 'processing'
  return 'neutral'
}

function useGraphSummary(libraryId: string) {
  return useQuery({
    ...queries.getKnowledgeLibrarySummaryOptions({ path: { libraryId } }),
  })
}

function GraphWorkbench({ libraryId }: Readonly<{ libraryId: string }>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const requestedNodeId = searchParams.get('nodeId')
  const summaryQuery = useGraphSummary(libraryId)
  const [retryToken, setRetryToken] = useState(0)
  const [nodes, setNodes] = useState<GraphNode[]>([])
  const [edges, setEdges] = useState<GraphEdge[]>([])
  const [metadata, setMetadata] = useState<GraphMetadata | null>(null)
  const [streamPhase, setStreamPhase] = useState<StreamPhase>('loading')
  const [streamError, setStreamError] = useState<string | null>(null)
  const [progress, setProgress] = useState<LoadProgress>({ nodes: 0, total: 0 })
  const [selectedId, setSelectedId] = useState<string | null>(requestedNodeId)
  const [search, setSearch] = useState('')
  const [debouncedSearch, setDebouncedSearch] = useState('')
  const [legendFilter, setLegendFilter] = useState<LegendFilter>(EMPTY_LEGEND_FILTER)
  const [focusedNeighborhoodId, setFocusedNeighborhoodId] = useState<string | null>(null)
  const [layout, setLayout] = useState<GraphLayoutType>(DEFAULT_GRAPH_LAYOUT)
  const [showDenseEdges, setShowDenseEdges] = useState(false)
  const [filtersOpen, setFiltersOpen] = useState(false)
  const fitViewRef = useRef<(() => void) | null>(null)

  useEffect(() => {
    const handle = window.setTimeout(() => setDebouncedSearch(search), 250)
    return () => window.clearTimeout(handle)
  }, [search])

  useEffect(() => {
    let cancelled = false

    // eslint-disable-next-line no-restricted-syntax -- NDJSON topology progress is not representable by a generated query option.
    void knowledgeApi
      .getGraphTopology(libraryId, {
        onProgress: (nextProgress) => {
          if (cancelled) return
          setProgress({
            nodes: nextProgress.nodesLoaded,
            total: nextProgress.expectedNodes,
          })
        },
      })
      .then((topology) => {
        if (cancelled) return
        const mapped = mapGraphTopology(topology)
        setNodes(mapped.nodes)
        setEdges(mapped.edges)
        setMetadata(mapped.meta)
        setProgress((current) => ({
          nodes: mapped.nodes.length,
          total: current.total || mapped.nodes.length,
        }))
        setStreamPhase('ready')
      })
      .catch((error: unknown) => {
        if (cancelled) return
        setStreamError(errorMessage(error, t('graph.failedToLoad')))
        setStreamPhase('error')
      })

    return () => {
      cancelled = true
    }
  }, [libraryId, retryToken, t])

  const adjacency = useGraphAdjacency(nodes, edges)

  const handleSelect = useCallback((nodeId: string | null) => {
    setSelectedId(nodeId)
    if (typeof window === 'undefined') return
    const next = new URLSearchParams(window.location.search)
    if (nodeId) next.set('nodeId', nodeId)
    else next.delete('nodeId')
    const query = next.toString()
    const search = query ? `?${query}` : ''
    window.history.replaceState(
      window.history.state,
      '',
      `${window.location.pathname}${search}${window.location.hash}`,
    )
  }, [])

  const handleFitViewReady = useCallback((fit: () => void) => {
    fitViewRef.current = fit
  }, [])

  useEffect(() => {
    if (!selectedId || filtersOpen) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.preventDefault()
      handleSelect(null)
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [filtersOpen, handleSelect, selectedId])

  const focusedIds = useMemo(() => {
    if (!focusedNeighborhoodId || !adjacency.nodeById.has(focusedNeighborhoodId)) return null
    return new Set([
      focusedNeighborhoodId,
      ...(adjacency.neighborIds.get(focusedNeighborhoodId) ?? []),
    ])
  }, [adjacency.neighborIds, adjacency.nodeById, focusedNeighborhoodId])

  const hiddenIds = useMemo(() => {
    const result = new Set<string>()
    const normalizedSearch = debouncedSearch.trim().toLocaleLowerCase(locale)
    for (const node of nodes) {
      if (
        isNodeHiddenByLegend(legendFilter, node) ||
        (normalizedSearch.length > 0 &&
          !node.label.toLocaleLowerCase(locale).includes(normalizedSearch)) ||
        (focusedIds && !focusedIds.has(node.id))
      ) {
        result.add(node.id)
      }
    }
    return result
  }, [debouncedSearch, focusedIds, legendFilter, locale, nodes])

  const typeLegend = useMemo(() => buildTypeLegend(nodes), [nodes])
  const showAll = useCallback(() => {
    setLegendFilter(EMPTY_LEGEND_FILTER)
    setFocusedNeighborhoodId(null)
  }, [])
  const selectedBasic = selectedId ? (adjacency.nodeById.get(selectedId) ?? null) : null
  const serverSummaryStatus = summaryStatus(summaryQuery.data)
  const graphStatus: GraphStatus = resolveGraphStatus({
    streamPhase,
    knownNodeCount: summaryQuery.data?.nodeCount ?? 0,
    serverStatus: serverSummaryStatus ?? metadata?.status ?? null,
    loadedNodeCount: nodes.length,
  })
  const nodeCount = summaryQuery.data?.nodeCount ?? metadata?.nodeCount ?? nodes.length
  const edgeCount = summaryQuery.data?.edgeCount ?? metadata?.edgeCount ?? edges.length
  const visibleNodeCount = nodes.length - hiddenIds.size
  const isBuilding = graphStatus === 'building' || graphStatus === 'rebuilding'

  const retry = () => {
    setNodes([])
    setEdges([])
    setMetadata(null)
    setStreamPhase('loading')
    setStreamError(null)
    setProgress({ nodes: 0, total: 0 })
    void summaryQuery.refetch()
    setRetryToken((current) => current + 1)
  }

  return (
    <div className="graph-page">
      <PageHeader title={t('graph.title')} description={t('graph.subtitle')} />

      <div className="graph-counts" aria-live="polite">
        <Badge tone={statusTone(graphStatus)}>{graphStatusLabel(graphStatus, t)}</Badge>
        <span>
          {formatNumber(nodeCount, locale)} {t('graph.nodes')}
        </span>
        <span>
          {formatNumber(edgeCount, locale)} {t('graph.edges')}
        </span>
        {hiddenIds.size > 0 && (
          <span>
            {formatNumber(hiddenIds.size, locale)} {t('graph.hidden')}
          </span>
        )}
      </div>

      {isBuilding && (
        <div className="graph-loading">
          <LoaderCircle className="spin" size={24} />
          <div>
            <strong>{t('graph.loading')}</strong>
            <span aria-live="polite">
              {formatNumber(progress.nodes, locale)} /{' '}
              {progress.total > 0 ? formatNumber(progress.total, locale) : '—'} {t('graph.nodes')}
            </span>
          </div>
          <Progress
            value={progress.total > 0 ? (progress.nodes / progress.total) * 100 : 0}
            label={t('graph.loading')}
          />
        </div>
      )}

      {!isBuilding && streamPhase === 'error' && (
        <div className="state-card state-card--error" role="alert">
          <AlertTriangle size={22} />
          <div>
            <strong>{t('graph.failedToLoad')}</strong>
            <p>{streamError}</p>
            <Button size="sm" onClick={retry}>
              {t('common.tryAgain')}
            </Button>
          </div>
        </div>
      )}

      {!isBuilding && streamPhase === 'ready' && nodes.length === 0 && (
        // Not a checkmark: "nothing here yet" is not a success. The tick beside
        // the text read as "all good" on a screen that has nothing to show.
        <div className="state-card">
          <Inbox size={22} />
          <div>
            <strong>{t('graph.noGraph')}</strong>
            <p>{t('graph.noGraphDesc', { productName: getBranding().displayName })}</p>
            <Button size="sm" onClick={() => navigate('/documents')}>
              <FileText size={14} />
              {t('graph.goToDocuments')}
            </Button>
          </div>
        </div>
      )}

      {!isBuilding && streamPhase === 'ready' && nodes.length > 0 && (
        <div className={`graph-workbench${selectedBasic ? ' is-inspecting' : ''}`}>
          <div className="graph-toolbar" role="toolbar" aria-label={t('graph.toolbar')}>
            <div className="search-box">
              <Search size={16} />
              <input
                className="input"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder={t('graph.searchPlaceholder')}
              />
            </div>
            <GraphLayoutPicker value={layout} onChange={setLayout} />
            {edges.length > GRAPH_EDGE_RENDER_CAP && (
              <Button
                size="sm"
                variant={showDenseEdges ? 'primary' : 'secondary'}
                aria-pressed={showDenseEdges}
                aria-label={t('graph.density')}
                title={
                  showDenseEdges ? t('graph.showSampledEdgesHint') : t('graph.showDenseEdgesHint')
                }
                onClick={() => setShowDenseEdges((current) => !current)}
              >
                <SlidersHorizontal size={14} />
                <span>{t('graph.density')}</span>
              </Button>
            )}
            <Button
              className="graph-filter-trigger"
              size="sm"
              aria-label={t('graph.filters')}
              onClick={() => setFiltersOpen(true)}
            >
              <ListFilter size={14} />
              <span>{t('graph.filters')}</span>
            </Button>
            <Button
              className="graph-fit-button"
              size="sm"
              aria-label={t('graph.fit')}
              title={t('graph.fit')}
              onClick={() => fitViewRef.current?.()}
            >
              <Maximize size={14} />
              <span>{t('graph.fit')}</span>
            </Button>
          </div>

          <GraphLegend
            legend={typeLegend}
            filter={legendFilter}
            onFilterChange={setLegendFilter}
            onShowAll={showAll}
          />
          <GraphCanvas
            nodes={nodes}
            edges={edges}
            hiddenIds={hiddenIds}
            selectedId={selectedId}
            layout={layout}
            showDenseEdges={showDenseEdges}
            showNoMatch={visibleNodeCount === 0}
            onSelect={handleSelect}
            onFitViewReady={handleFitViewReady}
          />
          <GraphInspector
            libraryId={libraryId}
            selectedBasic={selectedBasic}
            adjacency={adjacency}
            neighborhoodFocused={focusedNeighborhoodId === selectedId}
            onClose={() => handleSelect(null)}
            onSelectNode={(id) => handleSelect(id)}
            onFocusNeighborhood={(id) => {
              setFocusedNeighborhoodId((current) => (current === id ? null : id))
              setSearch('')
              setDebouncedSearch('')
              setLegendFilter(EMPTY_LEGEND_FILTER)
            }}
          />
        </div>
      )}

      <Drawer open={filtersOpen} onOpenChange={setFiltersOpen} title={t('graph.filters')}>
        <div className="graph-filter-drawer">
          <GraphLegend
            legend={typeLegend}
            filter={legendFilter}
            onFilterChange={setLegendFilter}
            onShowAll={showAll}
          />
        </div>
      </Drawer>
    </div>
  )
}

export default function GraphPage() {
  const { t } = useTranslation()
  const { activeLibrary } = useApp()
  const [searchParams, setSearchParams] = useSearchParams()
  const previousLibraryId = useRef<string | null>(null)
  const libraryId = activeLibrary?.id ?? null

  useEffect(() => {
    if (previousLibraryId.current && previousLibraryId.current !== libraryId) {
      const next = new URLSearchParams(searchParams)
      next.delete('nodeId')
      setSearchParams(next, { replace: true })
    }
    previousLibraryId.current = libraryId
  }, [libraryId, searchParams, setSearchParams])

  if (!libraryId) {
    return (
      <div className="graph-page">
        <PageHeader title={t('graph.title')} description={t('graph.subtitle')} />
        <div className="state-card state-card--stacked">
          <Inbox size={26} />
          <div>
            <strong>{t('graph.noLibrary')}</strong>
            <p>{t('graph.noLibraryDesc')}</p>
            <ChooseLibraryButton />
          </div>
        </div>
      </div>
    )
  }

  return <GraphWorkbench key={libraryId} libraryId={libraryId} />
}
import { getBranding } from '@/app/branding'
