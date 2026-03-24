import { defineStore } from 'pinia'
import type {
  GraphConvergenceStatus,
  GraphLayoutMode,
  GraphNode,
  GraphNodeType,
  GraphWorkspaceSurface,
} from 'src/models/ui/graph'
import {
  createGraphInspectorState,
  createGraphOverlayState,
} from 'src/components/graph/graphCanvasModel'
import { fetchGraphNodeDetail, fetchGraphSurface, searchGraphNodes } from 'src/services/api/graph'

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

  const documentMatch = nodes.find((node) => node.canonicalKey === `document:${identifier}`)
  if (documentMatch) {
    return documentMatch.id
  }

  return null
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
        filteredArtifactCount: 0,
        lastBuiltAt: null,
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
        !options?.preserveUi || !this.surface || previousLibraryId !== libraryId

      if (!this.surface) {
        this.surface = this.createEmptySurface()
      }

      if (shouldShowLoading && this.surface) {
        this.surface.loading = true
      }

      if (this.surface) {
        this.surface.error = null
      }

      if (!options?.preserveUi && this.surface) {
        this.surface.overlay.searchQuery = ''
        this.surface.overlay.searchHits = []
        this.surface.overlay.nodeTypeFilter = ''
        this.surface.overlay.activeLayout = 'cloud'
        this.surface.overlay.showFilteredArtifacts = false
      }

      try {
        const surface = await fetchGraphSurface(libraryId)
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        const preservedOverlay = this.surface?.overlay
        const preservedInspector = this.surface?.inspector
        const nextFocusedNodeId = resolveFocusedNodeId(
          surface.nodes,
          preservedInspector?.focusedNodeId ?? null,
        )
        this.surface = {
          ...surface,
          overlay: createGraphOverlayState({
            nodeCount: surface.nodeCount,
            edgeCount: surface.edgeCount,
            filteredArtifactCount: surface.filteredArtifactCount ?? 0,
            searchQuery: preservedOverlay?.searchQuery,
            searchHits: preservedOverlay?.searchHits,
            nodeTypeFilter: preservedOverlay?.nodeTypeFilter,
            activeLayout: preservedOverlay?.activeLayout,
            showFilteredArtifacts: preservedOverlay?.showFilteredArtifacts,
            showLegend: preservedOverlay?.showLegend,
            showFilters: preservedOverlay?.showFilters,
            zoomLevel: preservedOverlay?.zoomLevel,
          }),
          inspector: createGraphInspectorState({
            focusedNodeId: nextFocusedNodeId,
            detail: preservedInspector?.detail ?? null,
            loading: preservedInspector?.loading ?? false,
            error: preservedInspector?.error ?? null,
          }),
        }
        this.routeWarning = surface.warning ?? null
        await this.loadFocusedNodeDetail(libraryId, nextFocusedNodeId)
      } catch (error) {
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        if (this.surface) {
          this.surface.error =
            error instanceof Error ? error.message : 'Failed to load graph surface'
          this.surface.canvasMode = 'error'
        }
        throw error
      } finally {
        if (this.loadRequestId === requestId) {
          if (this.surface) {
            this.surface.loading = false
          }
        }
      }
    },
    async searchNodes(query: string): Promise<void> {
      if (!this.surface) {
        return
      }

      this.surface.overlay.searchQuery = query

      if (!this.activeLibraryId || !query.trim()) {
        this.surface.overlay.searchHits = []
        return
      }

      this.surface.overlay.searchHits = await searchGraphNodes(
        this.activeLibraryId,
        query,
        this.surface.nodes,
      )
    },
    async focusNode(identifier: string): Promise<void> {
      if (!this.surface) {
        return
      }
      const resolved = resolveFocusedNodeId(this.surface.nodes ?? [], identifier)
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
    },
    async setShowFilteredArtifacts(value: boolean): Promise<void> {
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
        const detail = await fetchGraphNodeDetail(
          resolvedLibraryId,
          this.surface.nodes ?? [],
          identifier,
        )
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
        if (this.surface) {
          this.surface.inspector = createGraphInspectorState({
            focusedNodeId: identifier,
            detail: null,
            loading: false,
            error: 'Failed to load node detail',
          })
        }
      } finally {
        if (this.detailRequestId === requestId) {
          if (this.surface) {
            this.surface.inspector.loading = false
          }
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
