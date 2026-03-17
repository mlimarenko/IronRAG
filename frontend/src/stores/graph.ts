import { defineStore } from 'pinia'
import type {
  GraphAssistantConfig,
  GraphConvergenceStatus,
  GraphDiagnostics,
  GraphLayoutMode,
  GraphNodeDetail,
  GraphQueryMode,
  GraphNodeType,
  GraphSearchHit,
  GraphSurfaceResponse,
} from 'src/models/ui/graph'
import {
  askGraphAssistant,
  fetchGraphAssistantConfig,
  fetchGraphDiagnostics,
  fetchGraphNodeDetail,
  fetchGraphSurface,
  searchGraphNodes,
} from 'src/services/api/graph'

interface GraphCanvasControls {
  fitViewport: (() => void) | null
  zoomIn: (() => void) | null
  zoomOut: (() => void) | null
}

interface GraphState {
  activeLibraryId: string | null
  surface: GraphSurfaceResponse | null
  assistantConfig: GraphAssistantConfig | null
  assistantConfigLibraryId: string | null
  diagnostics: GraphDiagnostics | null
  loading: boolean
  error: string | null
  assistantError: string | null
  searchQuery: string
  searchHits: GraphSearchHit[]
  nodeTypeFilter: GraphNodeType | ''
  showFilteredArtifacts: boolean
  layoutMode: GraphLayoutMode
  focusedNodeId: string | null
  focusedDetail: GraphNodeDetail | null
  detailLoading: boolean
  detailError: string | null
  assistantDraft: string
  assistantMode: GraphQueryMode
  assistantSubmitting: boolean
  controls: GraphCanvasControls
}

const FAST_REFRESH_INTERVAL_MS = 2_000
const WATCH_REFRESH_INTERVAL_MS = 4_000
const BACKLOG_REFRESH_INTERVAL_MS = 8_000
const WATCH_BACKLOG_THRESHOLD = 8
const THROTTLED_BACKLOG_THRESHOLD = 24

function resolveRefreshInterval(
  surface: GraphSurfaceResponse | null,
  diagnostics: GraphDiagnostics | null,
  convergenceStatus: GraphConvergenceStatus | null,
): number {
  const graphStatus = surface?.graphStatus ?? diagnostics?.graphStatus ?? null
  const rebuildBacklogCount = diagnostics?.rebuildBacklogCount ?? 0
  const readyNoGraphCount = diagnostics?.readyNoGraphCount ?? 0
  const pendingUpdateCount = diagnostics?.pendingUpdateCount ?? 0
  const pendingDeleteCount = diagnostics?.pendingDeleteCount ?? 0

  const needsPolling =
    graphStatus === 'building' ||
    graphStatus === 'stale' ||
    pendingUpdateCount > 0 ||
    pendingDeleteCount > 0 ||
    rebuildBacklogCount > 0 ||
    readyNoGraphCount > 0 ||
    convergenceStatus === 'partial'

  if (!needsPolling) {
    return 0
  }

  if (pendingUpdateCount > 0 || pendingDeleteCount > 0 || graphStatus === 'stale') {
    return FAST_REFRESH_INTERVAL_MS
  }

  const activeBacklogCount = rebuildBacklogCount + readyNoGraphCount
  if (activeBacklogCount >= THROTTLED_BACKLOG_THRESHOLD) {
    return BACKLOG_REFRESH_INTERVAL_MS
  }
  if (
    activeBacklogCount >= WATCH_BACKLOG_THRESHOLD ||
    graphStatus === 'building' ||
    convergenceStatus === 'partial'
  ) {
    return WATCH_REFRESH_INTERVAL_MS
  }

  return FAST_REFRESH_INTERVAL_MS
}

export const useGraphStore = defineStore('graph', {
  state: (): GraphState => ({
    activeLibraryId: null,
    surface: null,
    assistantConfig: null,
    assistantConfigLibraryId: null,
    diagnostics: null,
    loading: false,
    error: null,
    assistantError: null,
    searchQuery: '',
    searchHits: [],
    nodeTypeFilter: '',
    showFilteredArtifacts: false,
    layoutMode: 'cloud',
    focusedNodeId: null,
    focusedDetail: null,
    detailLoading: false,
    detailError: null,
    assistantDraft: '',
    assistantMode: 'hybrid',
    assistantSubmitting: false,
    controls: {
      fitViewport: null,
      zoomIn: null,
      zoomOut: null,
    },
  }),
  getters: {
    convergenceStatus(state): GraphConvergenceStatus | null {
      return state.surface?.convergenceStatus ?? state.diagnostics?.convergenceStatus ?? null
    },
    isPartiallyConverged(): boolean {
      return this.convergenceStatus === 'partial'
    },
    hasAdmittedOnlyTruth(state): boolean {
      return (
        state.focusedDetail?.activeProvenanceOnly ??
        state.diagnostics?.activeProvenanceOnly ??
        false
      )
    },
    activeBlockers(state): string[] {
      return state.diagnostics?.blockers ?? []
    },
    filteredArtifactCount(state): number {
      return (
        state.diagnostics?.filteredArtifactCount ??
        state.surface?.filteredArtifactCount ??
        state.focusedDetail?.filteredArtifactCount ??
        0
      )
    },
    refreshIntervalMs(state): number {
      return resolveRefreshInterval(
        state.surface,
        state.diagnostics,
        state.surface?.convergenceStatus ?? state.diagnostics?.convergenceStatus ?? null,
      )
    },
  },
  actions: {
    async ensureAssistantConfig(
      libraryId: string,
      options?: { force?: boolean },
    ): Promise<GraphAssistantConfig | null> {
      if (!options?.force && this.assistantConfigLibraryId === libraryId) {
        return this.assistantConfig
      }

      const assistantConfig = await fetchGraphAssistantConfig(libraryId).catch(() => null)
      this.assistantConfig = assistantConfig
      this.assistantConfigLibraryId = libraryId
      return assistantConfig
    },
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
        this.assistantError = null
        this.searchHits = []
        this.assistantDraft = ''
      }
      try {
        const assistantConfigPromise = this.ensureAssistantConfig(libraryId)
        const [surface, diagnostics] = await Promise.all([
          fetchGraphSurface({ includeFiltered: this.showFilteredArtifacts }),
          fetchGraphDiagnostics(),
        ])
        await assistantConfigPromise
        this.surface = surface
        this.diagnostics = diagnostics
        if (this.focusedNodeId) {
          try {
            await this.focusNode(this.focusedNodeId)
          } catch {
            this.focusedNodeId = null
            this.focusedDetail = null
          }
        } else {
          this.focusedDetail = null
        }
      } catch (error) {
        this.diagnostics = null
        this.error = error instanceof Error ? error.message : 'Failed to load graph surface'
        throw error
      } finally {
        this.loading = false
      }
    },
    async searchNodes(query: string): Promise<void> {
      this.searchQuery = query
      if (!query.trim()) {
        this.searchHits = []
        return
      }
      this.searchHits = await searchGraphNodes(query, {
        includeFiltered: this.showFilteredArtifacts,
      })
    },
    async focusNode(id: string): Promise<void> {
      this.focusedNodeId = id
      this.error = null
      this.detailLoading = true
      this.detailError = null
      try {
        this.focusedDetail = await fetchGraphNodeDetail(id, {
          includeFiltered: this.showFilteredArtifacts,
        })
      } catch (error) {
        this.focusedDetail = null
        this.detailError =
          error instanceof Error ? error.message : 'Failed to load graph node detail'
      } finally {
        this.detailLoading = false
      }
    },
    clearFocus(): void {
      this.focusedNodeId = null
      this.focusedDetail = null
      this.detailLoading = false
      this.detailError = null
    },
    setNodeTypeFilter(value: GraphNodeType | ''): void {
      this.nodeTypeFilter = value
    },
    async setShowFilteredArtifacts(value: boolean): Promise<void> {
      this.showFilteredArtifacts = value
      this.searchHits = []
      if (this.activeLibraryId) {
        await this.loadSurface(this.activeLibraryId, { preserveUi: true })
      }
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
    setAssistantMode(mode: GraphQueryMode): void {
      this.assistantMode = mode
    },
    async submitAssistantPrompt(question: string): Promise<void> {
      if (!this.surface || !question.trim()) {
        return
      }

      this.assistantSubmitting = true
      this.assistantError = null
      try {
        const answer = await askGraphAssistant(
          question.trim(),
          this.surface.assistant.sessionId,
          this.focusedNodeId,
          this.assistantMode,
        )

        this.surface.assistant.sessionId = answer.sessionId
        this.surface.assistant.messages = [
          ...this.surface.assistant.messages,
          {
            id: answer.userMessageId,
            role: 'user',
            content: question.trim(),
            createdAt: new Date().toISOString(),
            queryId: null,
            mode: this.assistantMode,
            groundingStatus: null,
            provider: null,
            references: [],
            planning: null,
            rerank: null,
            contextAssembly: null,
            warning: null,
            warningKind: null,
          },
          {
            id: answer.assistantMessageId,
            role: 'assistant',
            content: answer.answer,
            createdAt: new Date().toISOString(),
            queryId: answer.queryId,
            mode: answer.mode,
            groundingStatus: answer.groundingStatus,
            provider: answer.provider,
            references: answer.structuredReferences,
            planning: answer.planning,
            rerank: answer.rerank,
            contextAssembly: answer.contextAssembly,
            warning: answer.warning,
            warningKind: answer.warningKind,
          },
        ]
        this.assistantDraft = ''
      } catch (error) {
        this.assistantError =
          error instanceof Error ? error.message : 'Failed to ask the graph assistant'
      } finally {
        this.assistantSubmitting = false
      }
    },
  },
})
