import { defineStore } from 'pinia'
import type {
  GraphAssistantState,
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
import type {
  ChatSettingsDraft,
  ChatSessionSettings,
  ChatSessionSummary,
  ChatThreadMessage,
} from 'src/models/ui/chat'
import {
  buildChatSettingsDraft,
  decorateChatThreadMessage,
  updateChatSettingsDraft,
} from 'src/models/ui/chat'
import {
  createChatSession,
  fetchChatSession,
  fetchChatSessionMessages,
  listChatSessions,
  updateChatSession,
} from 'src/services/api/chat'
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
  activeSessionId: string | null
  recentSessions: ChatSessionSummary[]
  sessionLoading: boolean
  sessionError: string | null
  assistantSettingsOpen: boolean
  assistantSettingsSaving: boolean
  assistantSettings: ChatSessionSettings | null
  assistantSettingsDraft: ChatSettingsDraft | null
  sourceDisclosureState: Record<string, boolean>
  controls: GraphCanvasControls
}

const FAST_REFRESH_INTERVAL_MS = 2_000
const WATCH_REFRESH_INTERVAL_MS = 4_000
const BACKLOG_REFRESH_INTERVAL_MS = 8_000
const WATCH_BACKLOG_THRESHOLD = 8
const THROTTLED_BACKLOG_THRESHOLD = 24

function syncAssistantState(
  state: GraphState,
  assistant: GraphAssistantState | null,
  options?: { preserveDraft?: boolean },
): void {
  state.activeSessionId = assistant?.activeSession?.sessionId ?? assistant?.sessionId ?? null
  state.recentSessions = assistant?.recentSessions ?? []
  state.assistantSettings = assistant?.settingsSummary ?? null
  if (assistant?.settingsSummary) {
    state.assistantMode = assistant.settingsSummary.preferredMode
  }
  if (!options?.preserveDraft) {
    state.assistantSettingsDraft = assistant?.settingsSummary
      ? buildChatSettingsDraft(assistant.settingsSummary)
      : null
  }
}

function createPendingAssistantMessage(): ChatThreadMessage {
  return decorateChatThreadMessage({
    id: `pending-assistant-${String(Date.now())}`,
    role: 'assistant',
    content: '',
    createdAt: new Date().toISOString(),
    queryId: null,
    mode: null,
    groundingStatus: null,
    provider: null,
    references: [],
    planning: null,
    rerank: null,
    contextAssembly: null,
    warning: null,
    warningKind: null,
    pending: true,
  })
}

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
    activeSessionId: null,
    recentSessions: [],
    sessionLoading: false,
    sessionError: null,
    assistantSettingsOpen: false,
    assistantSettingsSaving: false,
    assistantSettings: null,
    assistantSettingsDraft: null,
    sourceDisclosureState: {},
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
        this.sessionError = null
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
        if (options?.preserveUi && this.surface && this.activeSessionId) {
          surface.assistant = {
            ...surface.assistant,
            sessionId: this.surface.assistant.sessionId,
            recentSessions: this.recentSessions,
            activeSession: this.surface.assistant.activeSession,
            settingsSummary: this.assistantSettings,
            focusContext: this.surface.assistant.focusContext,
            messages: this.surface.assistant.messages,
          }
        }
        this.surface = surface
        this.diagnostics = diagnostics
        syncAssistantState(this, surface.assistant, {
          preserveDraft: this.assistantSettingsOpen,
        })
        if (this.focusedNodeId) {
          const shouldRefreshFocusedDetail =
            previousLibraryId !== libraryId ||
            !options?.preserveUi ||
            this.focusedDetail?.id !== this.focusedNodeId

          if (shouldRefreshFocusedDetail) {
            try {
              await this.focusNode(this.focusedNodeId)
            } catch {
              this.focusedNodeId = null
              this.focusedDetail = null
            }
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
      if (this.assistantSettingsDraft) {
        this.assistantSettingsDraft = updateChatSettingsDraft(this.assistantSettingsDraft, {
          preferredMode: mode,
        })
      }
    },
    async loadRecentChats(): Promise<void> {
      if (!this.activeLibraryId) {
        return
      }
      this.sessionLoading = true
      this.sessionError = null
      try {
        const sessions = await listChatSessions(this.activeLibraryId)
        this.recentSessions = sessions
        if (this.surface) {
          this.surface.assistant.recentSessions = sessions
        }
      } catch (error) {
        this.sessionError = error instanceof Error ? error.message : 'Failed to load recent chats'
      } finally {
        this.sessionLoading = false
      }
    },
    async loadChatSession(sessionId: string): Promise<void> {
      if (!this.surface) {
        return
      }
      this.sessionLoading = true
      this.sessionError = null
      try {
        const [envelope, messages] = await Promise.all([
          fetchChatSession(sessionId),
          fetchChatSessionMessages(sessionId),
        ])

        this.activeSessionId = envelope.session.sessionId
        this.assistantMode = envelope.settings.preferredMode
        this.assistantSettings = envelope.settings
        this.assistantSettingsDraft = buildChatSettingsDraft(envelope.settings)
        this.surface.assistant = {
          ...this.surface.assistant,
          sessionId: envelope.session.sessionId,
          activeSession: envelope.session,
          settingsSummary: envelope.settings,
          messages: messages.map((message) => decorateChatThreadMessage(message)),
        }
        await this.loadRecentChats()
      } catch (error) {
        this.sessionError =
          error instanceof Error ? error.message : 'Failed to load chat session history'
      } finally {
        this.sessionLoading = false
      }
    },
    async createNewChat(): Promise<void> {
      if (!this.activeLibraryId || !this.surface) {
        return
      }
      this.sessionLoading = true
      this.sessionError = null
      this.assistantError = null
      try {
        const envelope = await createChatSession(this.activeLibraryId)
        this.assistantDraft = ''
        this.activeSessionId = envelope.session.sessionId
        this.assistantMode = envelope.settings.preferredMode
        this.assistantSettings = envelope.settings
        this.assistantSettingsDraft = buildChatSettingsDraft(envelope.settings)
        this.surface.assistant = {
          ...this.surface.assistant,
          sessionId: envelope.session.sessionId,
          activeSession: envelope.session,
          settingsSummary: envelope.settings,
          messages: [],
        }
        await this.loadRecentChats()
      } catch (error) {
        this.sessionError = error instanceof Error ? error.message : 'Failed to create chat'
      } finally {
        this.sessionLoading = false
      }
    },
    openAssistantSettings(): void {
      if (this.assistantSettings) {
        this.assistantSettingsDraft = buildChatSettingsDraft(this.assistantSettings)
      }
      this.assistantSettingsOpen = true
    },
    closeAssistantSettings(): void {
      this.assistantSettingsOpen = false
      if (this.assistantSettings) {
        this.assistantSettingsDraft = buildChatSettingsDraft(this.assistantSettings)
      }
    },
    updateAssistantSettingsDraft(
      patch: Partial<Pick<ChatSettingsDraft, 'systemPrompt' | 'preferredMode'>>,
    ): void {
      this.assistantSettingsDraft ??=
        this.assistantSettings
          ? buildChatSettingsDraft(this.assistantSettings)
          : {
              systemPrompt: '',
              preferredMode: this.assistantMode,
              initialSystemPrompt: '',
              initialPreferredMode: this.assistantMode,
              isDirty: false,
              canRestoreDefault: true,
              validationError: null,
            }
      this.assistantSettingsDraft = updateChatSettingsDraft(this.assistantSettingsDraft, patch)
    },
    async saveAssistantSettings(options?: { restoreDefault?: boolean }): Promise<void> {
      if (!this.activeSessionId || !this.assistantSettingsDraft || !this.surface) {
        return
      }
      if (this.assistantSettingsDraft.validationError) {
        this.sessionError = this.assistantSettingsDraft.validationError
        return
      }
      this.assistantSettingsSaving = true
      this.sessionError = null
      try {
        const envelope = await updateChatSession(this.activeSessionId, {
          system_prompt: this.assistantSettingsDraft.systemPrompt,
          prompt_state: this.assistantSettings?.promptState,
          preferred_mode: this.assistantSettingsDraft.preferredMode,
          restore_default: options?.restoreDefault ?? false,
        })
        this.assistantSettings = envelope.settings
        this.assistantSettingsDraft = buildChatSettingsDraft(envelope.settings)
        this.assistantMode = envelope.settings.preferredMode
        this.surface.assistant = {
          ...this.surface.assistant,
          activeSession: envelope.session,
          settingsSummary: envelope.settings,
        }
        this.assistantSettingsOpen = false
        await this.loadRecentChats()
      } catch (error) {
        this.sessionError =
          error instanceof Error ? error.message : 'Failed to update chat settings'
      } finally {
        this.assistantSettingsSaving = false
      }
    },
    toggleMessageSources(messageId: string): void {
      this.sourceDisclosureState[messageId] = !this.sourceDisclosureState[messageId]
    },
    async submitAssistantPrompt(question: string): Promise<void> {
      if (!this.surface || !question.trim()) {
        return
      }

      this.assistantSubmitting = true
      this.assistantError = null
      const trimmedQuestion = question.trim()
      const optimisticUserMessage = decorateChatThreadMessage({
        id: `pending-user-${String(Date.now())}`,
        role: 'user',
        content: trimmedQuestion,
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
      })
      const pendingAssistantMessage = createPendingAssistantMessage()
      const threadBeforeSubmit = [...this.surface.assistant.messages]
      this.surface.assistant.messages = [
        ...threadBeforeSubmit,
        optimisticUserMessage,
        pendingAssistantMessage,
      ]
      try {
        const answer = await askGraphAssistant(
          trimmedQuestion,
          this.activeSessionId ?? this.surface.assistant.sessionId,
          this.focusedNodeId,
          this.assistantMode,
        )

        this.surface.assistant.sessionId = answer.sessionId
        this.activeSessionId = answer.sessionId
        this.surface.assistant.messages = [
          ...threadBeforeSubmit,
          decorateChatThreadMessage(answer.userMessage),
          decorateChatThreadMessage(answer.assistantMessage),
        ]
        if (answer.sessionSummary) {
          this.surface.assistant.activeSession = answer.sessionSummary
        }
        if (answer.settingsSummary) {
          this.assistantSettings = answer.settingsSummary
          this.assistantSettingsDraft = buildChatSettingsDraft(answer.settingsSummary)
          this.surface.assistant.settingsSummary = answer.settingsSummary
        } else {
          const sessionEnvelope = await fetchChatSession(answer.sessionId)
          this.assistantSettings = sessionEnvelope.settings
          this.assistantSettingsDraft = buildChatSettingsDraft(sessionEnvelope.settings)
          this.surface.assistant.activeSession = sessionEnvelope.session
          this.surface.assistant.settingsSummary = sessionEnvelope.settings
        }
        this.assistantMode = answer.effectiveMode
        await this.loadRecentChats()
        this.assistantDraft = ''
      } catch (error) {
        this.surface.assistant.messages = [
          ...threadBeforeSubmit,
          optimisticUserMessage,
          decorateChatThreadMessage({
            ...pendingAssistantMessage,
            id: pendingAssistantMessage.id,
            content:
              error instanceof Error ? error.message : 'Failed to ask the graph assistant',
            warning: error instanceof Error ? error.message : 'Failed to ask the graph assistant',
            warningKind: 'request_failed',
            pending: false,
          }),
        ]
        this.assistantError =
          error instanceof Error ? error.message : 'Failed to ask the graph assistant'
      } finally {
        this.assistantSubmitting = false
      }
    },
  },
})
