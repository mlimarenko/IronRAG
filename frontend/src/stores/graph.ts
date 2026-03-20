import { defineStore } from 'pinia'
import type {
  GraphConvergenceStatus,
  GraphLayoutMode,
  GraphNode,
  GraphNodeType,
  GraphSearchHit,
  GraphSurfaceResponse,
} from 'src/models/ui/graph'
import { fetchGraphSurface, searchGraphNodes } from 'src/services/api/graph'

interface GraphCanvasControls {
  fitViewport: (() => void) | null
  zoomIn: (() => void) | null
  zoomOut: (() => void) | null
}

interface GraphState {
  activeLibraryId: string | null
  surface: GraphSurfaceResponse | null
  loading: boolean
  error: string | null
  searchQuery: string
  searchHits: GraphSearchHit[]
  nodeTypeFilter: GraphNodeType | ''
  showFilteredArtifacts: boolean
  layoutMode: GraphLayoutMode
  focusedNodeId: string | null
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
    loading: false,
    error: null,
    searchQuery: '',
    searchHits: [],
    nodeTypeFilter: '',
    showFilteredArtifacts: false,
    layoutMode: 'cloud',
    focusedNodeId: null,
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
      return state.surface?.graphStatus === 'building' || state.surface?.graphStatus === 'partial'
        ? BUILDING_REFRESH_INTERVAL_MS
        : 0
    },
  },
  actions: {
    async loadSurface(libraryId: string, options?: { preserveUi?: boolean }): Promise<void> {
      const previousLibraryId = this.activeLibraryId
      this.activeLibraryId = libraryId
      const shouldShowLoading =
        !options?.preserveUi || !this.surface || previousLibraryId !== libraryId

      if (shouldShowLoading) {
        this.loading = true
      }

      this.error = null

      if (!options?.preserveUi) {
        this.searchHits = []
        this.searchQuery = ''
      }

      try {
        const surface = await fetchGraphSurface(libraryId)
        this.surface = surface
        this.focusedNodeId = resolveFocusedNodeId(surface.nodes, this.focusedNodeId)
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load graph surface'
        throw error
      } finally {
        this.loading = false
      }
    },
    async searchNodes(query: string): Promise<void> {
      this.searchQuery = query

      if (!this.activeLibraryId || !this.surface || !query.trim()) {
        this.searchHits = []
        return
      }

      this.searchHits = await searchGraphNodes(this.activeLibraryId, query, this.surface.nodes)
    },
    async focusNode(identifier: string): Promise<void> {
      const resolved = resolveFocusedNodeId(this.surface?.nodes ?? [], identifier)
      this.focusedNodeId = resolved
    },
    clearFocus(): void {
      this.focusedNodeId = null
    },
    setNodeTypeFilter(value: GraphNodeType | ''): void {
      this.nodeTypeFilter = value
    },
    async setShowFilteredArtifacts(value: boolean): Promise<void> {
      this.showFilteredArtifacts = value
    },
    setLayoutMode(value: GraphLayoutMode): void {
      this.layoutMode = value
    },
    registerCanvasControls(controls: GraphCanvasControls): void {
      this.controls = controls
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
