import { defineStore } from 'pinia'
import type {
  GraphConvergenceStatus,
  GraphLayoutMode,
  GraphNode,
  GraphNodeType,
  GraphWorkspaceSurface,
} from 'src/models/ui/graph'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'
import {
  createGraphInspectorState,
  createGraphOverlayState,
} from 'src/components/graph/graphCanvasModel'
import {
  fetchGraphNodeDetail,
  fetchGraphSurface,
  fetchGraphSurfaceHeartbeat,
  searchGraphNodes,
} from 'src/services/api/graph'

interface GraphCanvasControls {
  fitViewport: (() => void) | null
  zoomIn: (() => void) | null
  zoomOut: (() => void) | null
}

interface GraphState {
  activeLibraryId: string | null
  surface: GraphWorkspaceSurface | null
  routeWarning: string | null
  loadRequestId: number
  detailRequestId: number
  controls: GraphCanvasControls
}

const BUILDING_REFRESH_INTERVAL_MS = 4_000

function resolveFocusedNodeId(nodes: GraphNode[], identifier: string | null): string | null {
  if (!identifier) {
    return null
  }

  const exactMatch = nodes.find((node) => node.id === identifier)
  if (exactMatch) {
    return exactMatch.id
  }

  const canonicalMatch = nodes.find((node) => node.canonicalKey === identifier)
  if (canonicalMatch) {
    return canonicalMatch.id
  }

  return (
    nodes.find((node) => node.canonicalKey === `document:${identifier}`)?.id ??
    nodes.find((node) => node.canonicalKey === `entity:${identifier}`)?.id ??
    nodes.find((node) => node.canonicalKey === `relation:${identifier}`)?.id ??
    null
  )
}

export const useGraphStore = defineStore('graph', {
  state: (): GraphState => ({
    activeLibraryId: null,
    surface: null,
    routeWarning: null,
    loadRequestId: 0,
    detailRequestId: 0,
    controls: {
      fitViewport: null,
      zoomIn: null,
      zoomOut: null,
    },
  }),
  getters: {
    convergenceStatus(state): GraphConvergenceStatus | null {
      return state.surface?.convergenceStatus ?? null
    },
    isPartiallyConverged(): boolean {
      return this.convergenceStatus === 'partial'
    },
    hasAdmittedOnlyTruth(): boolean {
      return false
    },
    filteredArtifactCount(state): number {
      return state.surface?.filteredArtifactCount ?? 0
    },
    refreshIntervalMs(state): number {
      return state.surface?.graphStatus === 'building' ||
        state.surface?.graphStatus === 'partial' ||
        state.surface?.graphStatus === 'rebuilding'
        ? BUILDING_REFRESH_INTERVAL_MS
        : 0
    },
  },
  actions: {
    syncSearchHits(): void {
      if (!this.surface) {
        return
      }

      const { searchQuery, nodeTypeFilter } = this.surface.overlay
      if (!searchQuery.trim()) {
        this.surface.overlay.searchHits = []
        return
      }

      this.surface.overlay.searchHits = searchGraphNodes(
        searchQuery,
        this.surface.nodes,
        nodeTypeFilter,
      )
    },
    createEmptySurface(): GraphWorkspaceSurface {
      return {
        loading: false,
        error: null,
        canvasMode: 'empty',
        graphStatus: 'empty',
        convergenceStatus: null,
        graphGeneration: 0,
        graphGenerationState: null,
        nodeCount: 0,
        relationCount: 0,
        edgeCount: 0,
        hiddenNodeCount: 0,
        filteredArtifactCount: 0,
        lastBuiltAt: null,
        readinessSummary: null,
        graphCoverage: null,
        warning: null,
        nodes: [],
        edges: [],
        legend: [],
        overlay: createGraphOverlayState({
          nodeCount: 0,
          edgeCount: 0,
          filteredArtifactCount: 0,
        }),
        inspector: createGraphInspectorState(),
      }
    },
    async loadSurface(libraryId: string, options?: { preserveUi?: boolean }): Promise<void> {
      const previousLibraryId = this.activeLibraryId
      this.activeLibraryId = libraryId
      const requestId = ++this.loadRequestId
      const shouldShowLoading =
        !options?.preserveUi || this.surface === null || previousLibraryId !== libraryId

      if (!this.surface) {
        this.surface = this.createEmptySurface()
      }
      const currentSurface = this.surface

      if (shouldShowLoading) {
        currentSurface.loading = true
      }
      currentSurface.error = null

      if (!options?.preserveUi) {
        currentSurface.overlay.searchQuery = ''
        currentSurface.overlay.searchHits = []
        currentSurface.overlay.nodeTypeFilter = ''
        currentSurface.overlay.activeLayout = resolveDefaultGraphLayoutMode(
          currentSurface.nodeCount,
          currentSurface.edgeCount,
        )
        currentSurface.overlay.showFilteredArtifacts = false
      }

      try {
        const surface = await fetchGraphSurface(libraryId)
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        const preservedOverlay = currentSurface.overlay
        const preservedInspector = currentSurface.inspector
        const nextFocusedNodeId = resolveFocusedNodeId(
          surface.nodes,
          preservedInspector.focusedNodeId,
        )
        this.surface = {
          ...surface,
          overlay: createGraphOverlayState({
            nodeCount: surface.nodeCount,
            edgeCount: surface.edgeCount,
            filteredArtifactCount: surface.filteredArtifactCount ?? 0,
            searchQuery: preservedOverlay.searchQuery,
            searchHits: [],
            nodeTypeFilter: preservedOverlay.nodeTypeFilter,
            activeLayout: options?.preserveUi
              ? preservedOverlay.activeLayout
              : surface.overlay.activeLayout,
            showFilteredArtifacts: preservedOverlay.showFilteredArtifacts,
            showLegend: preservedOverlay.showLegend,
            showFilters: preservedOverlay.showFilters,
            zoomLevel: preservedOverlay.zoomLevel,
          }),
          inspector: createGraphInspectorState({
            focusedNodeId: nextFocusedNodeId,
            detail: preservedInspector.detail,
            loading: preservedInspector.loading,
            error: preservedInspector.error,
          }),
        }
        this.syncSearchHits()
        this.routeWarning = surface.warning
        await this.loadFocusedNodeDetail(libraryId, nextFocusedNodeId)
      } catch (error) {
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        currentSurface.error =
          error instanceof Error ? error.message : 'Failed to load graph surface'
        currentSurface.canvasMode = 'error'
        throw error
      } finally {
        if (this.loadRequestId === requestId) {
          currentSurface.loading = false
        }
      }
    },
    async pollSurface(libraryId: string): Promise<void> {
      if (!this.surface || this.surface.loading) {
        await this.loadSurface(libraryId, { preserveUi: true })
        return
      }

      const previousStatus = this.surface.graphStatus
      const previousGeneration = this.surface.graphGeneration
      const previousGenerationState = this.surface.graphGenerationState
      const previousLastBuiltAt = this.surface.lastBuiltAt
      const previousCanvasMode = this.surface.canvasMode
      const previousNodeCount = this.surface.nodeCount
      const previousEdgeCount = this.surface.edgeCount

      const heartbeat = await fetchGraphSurfaceHeartbeat(
        libraryId,
        this.surface.nodeCount,
        this.surface.relationCount,
        {
          graphStatus: this.surface.graphStatus,
          convergenceStatus: this.surface.convergenceStatus,
          graphGeneration: this.surface.graphGeneration,
          graphGenerationState: this.surface.graphGenerationState ?? null,
          lastBuiltAt: this.surface.lastBuiltAt,
          readinessSummary: this.surface.readinessSummary,
          graphCoverage: this.surface.graphCoverage,
          warning: this.surface.warning,
        },
      )

      if (this.activeLibraryId !== libraryId) {
        return
      }

      this.surface.graphStatus = heartbeat.graphStatus
      this.surface.convergenceStatus = heartbeat.convergenceStatus
      this.surface.graphGeneration = heartbeat.graphGeneration
      this.surface.graphGenerationState = heartbeat.graphGenerationState
      this.surface.lastBuiltAt = heartbeat.lastBuiltAt
      this.surface.readinessSummary = heartbeat.readinessSummary
      this.surface.graphCoverage = heartbeat.graphCoverage
      this.surface.warning = heartbeat.warning
      this.routeWarning = heartbeat.warning

      const generationChanged = heartbeat.graphGeneration !== previousGeneration
      const generationStateChanged = heartbeat.graphGenerationState !== previousGenerationState
      const lastBuiltAtChanged = heartbeat.lastBuiltAt !== previousLastBuiltAt
      const statusSettled =
        heartbeat.graphStatus === 'ready' ||
        heartbeat.graphStatus === 'failed' ||
        heartbeat.graphStatus === 'stale'
      const statusChanged = heartbeat.graphStatus !== previousStatus
      const staleSparseSurface =
        (previousCanvasMode === 'empty' || previousCanvasMode === 'sparse') &&
        heartbeat.graphStatus !== 'empty' &&
        heartbeat.graphStatus !== 'building' &&
        (previousNodeCount === 0 || previousEdgeCount === 0)

      if (
        generationChanged ||
        lastBuiltAtChanged ||
        staleSparseSurface ||
        (statusSettled && (statusChanged || generationStateChanged))
      ) {
        await this.loadSurface(libraryId, { preserveUi: true })
      }
    },
    searchNodes(query: string): void {
      if (!this.surface) {
        return
      }

      this.surface.overlay.searchQuery = query
      this.syncSearchHits()
    },
    clearSearch(): void {
      if (!this.surface) {
        return
      }
      this.surface.overlay.searchQuery = ''
      this.surface.overlay.searchHits = []
    },
    async focusNode(identifier: string): Promise<void> {
      if (!this.surface) {
        return
      }
      const resolved = resolveFocusedNodeId(this.surface.nodes, identifier)
      this.surface.inspector.focusedNodeId = resolved
      await this.loadFocusedNodeDetail(this.activeLibraryId, resolved)
    },
    clearFocus(): void {
      if (!this.surface) {
        return
      }
      this.surface.inspector = createGraphInspectorState()
    },
    setNodeTypeFilter(value: GraphNodeType | ''): void {
      if (!this.surface) {
        return
      }
      this.surface.overlay.nodeTypeFilter = value
      this.syncSearchHits()
    },
    setShowFilteredArtifacts(value: boolean): void {
      if (!this.surface) {
        return
      }
      this.surface.overlay.showFilteredArtifacts = value
    },
    setLayoutMode(value: GraphLayoutMode): void {
      if (!this.surface) {
        return
      }
      this.surface.overlay.activeLayout = value
    },
    registerCanvasControls(controls: GraphCanvasControls): void {
      this.controls = controls
    },
    async loadFocusedNodeDetail(
      libraryId?: string | null,
      identifier?: string | null,
    ): Promise<void> {
      const resolvedLibraryId = libraryId ?? this.activeLibraryId
      if (!this.surface || !resolvedLibraryId || !identifier) {
        if (this.surface) {
          this.surface.inspector = createGraphInspectorState({
            focusedNodeId: null,
            loading: false,
            error: null,
            detail: null,
          })
        }
        return
      }

      const requestId = ++this.detailRequestId
      this.surface.inspector.loading = true
      this.surface.inspector.error = null

      try {
        const detail = await fetchGraphNodeDetail(resolvedLibraryId, this.surface.nodes, identifier)
        if (this.detailRequestId !== requestId || this.activeLibraryId !== resolvedLibraryId) {
          return
        }
        this.surface.inspector = createGraphInspectorState({
          focusedNodeId: identifier,
          detail,
          loading: false,
          error: null,
        })
      } catch {
        if (this.detailRequestId !== requestId) {
          return
        }
        this.surface.inspector = createGraphInspectorState({
          focusedNodeId: identifier,
          detail: null,
          loading: false,
          error: 'Failed to load node detail',
        })
      } finally {
        if (this.detailRequestId === requestId) {
          this.surface.inspector.loading = false
        }
      }
    },
    fitViewport(): void {
      this.controls.fitViewport?.()
    },
    zoomIn(): void {
      this.controls.zoomIn?.()
    },
    zoomOut(): void {
      this.controls.zoomOut?.()
    },
  },
})
