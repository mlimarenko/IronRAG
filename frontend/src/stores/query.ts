import { defineStore } from 'pinia'
import { ApiClientError } from 'src/services/api/http'
import {
  createQuerySession,
  executeQueryTurn,
  type ExecuteQueryTurnStreamHandlers,
  fetchKnowledgeContextBundle,
  fetchQueryExecutionDetail,
  fetchQuerySessionDetail,
  listQuerySessions,
  type CreateQuerySessionPayload,
  type ExecuteQueryTurnPayload,
  type KnowledgeBundleChunkReference,
  type KnowledgeBundleEntityReference,
  type KnowledgeBundleEvidenceReference,
  type KnowledgeBundleRelationReference,
  type KnowledgeContextBundleDetail,
  type QueryExecution,
  type QueryExecutionDetail,
  type QueryRuntimeStageSummary,
  type QueryPreparedSegmentReference,
  type RuntimeExecutionSummary,
  type QuerySession,
  type QuerySessionDetail,
  type QueryTechnicalFactReference,
  type QueryTurn,
  type QueryVerificationState,
  type QueryVerificationWarning,
} from 'src/services/api/query'
import { useShellStore } from './shell'

interface QueryState {
  activeLibraryId: string | null
  sessions: QuerySession[]
  activeSession: QuerySessionDetail | null
  activeExecution: QueryExecutionDetail | null
  activeRuntimeSummary: RuntimeExecutionSummary | null
  activeRuntimeStageSummaries: QueryRuntimeStageSummary[]
  activeBundle: KnowledgeContextBundleDetail | null
  loadingSessions: boolean
  loadingSession: boolean
  loadingExecution: boolean
  executingTurn: boolean
  error: string | null
  graphSurfacePriority: 'primary' | 'secondary'
}

function resolveShellScope(): { workspaceId: string; libraryId: string } | null {
  const shell = useShellStore()
  const workspaceId = shell.activeWorkspace?.id ?? null
  const libraryId = shell.activeLibrary?.id ?? null
  if (!workspaceId || !libraryId) {
    return null
  }
  return { workspaceId, libraryId }
}

function normalizeErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof ApiClientError || error instanceof Error) {
    return error.message
  }
  return fallback
}

export const useQueryStore = defineStore('query', {
  state: (): QueryState => ({
    activeLibraryId: null,
    sessions: [],
    activeSession: null,
    activeExecution: null,
    activeRuntimeSummary: null,
    activeRuntimeStageSummaries: [],
    activeBundle: null,
    loadingSessions: false,
    loadingSession: false,
    loadingExecution: false,
    executingTurn: false,
    error: null,
    graphSurfacePriority: 'secondary',
  }),
  getters: {
    activeTurns(state): QueryTurn[] {
      return state.activeSession?.turns ?? []
    },
    activeExecutions(state): QueryExecution[] {
      return state.activeSession?.executions ?? []
    },
    groundedChunkReferences(state): KnowledgeBundleChunkReference[] {
      return state.activeBundle?.chunkReferences ?? []
    },
    groundedPreparedSegmentReferences(state): QueryPreparedSegmentReference[] {
      return state.activeExecution?.preparedSegmentReferences ?? []
    },
    groundedTechnicalFactReferences(state): QueryTechnicalFactReference[] {
      return state.activeExecution?.technicalFactReferences ?? []
    },
    groundedEntityReferences(state): KnowledgeBundleEntityReference[] {
      return state.activeBundle?.entityReferences ?? []
    },
    groundedRelationReferences(state): KnowledgeBundleRelationReference[] {
      return state.activeBundle?.relationReferences ?? []
    },
    groundedEvidenceReferences(state): KnowledgeBundleEvidenceReference[] {
      return state.activeBundle?.evidenceReferences ?? []
    },
    verificationState(state): QueryVerificationState {
      return state.activeExecution?.verificationState ?? 'not_run'
    },
    verificationWarnings(state): QueryVerificationWarning[] {
      return state.activeExecution?.verificationWarnings ?? []
    },
    runtimeSummary(state): RuntimeExecutionSummary | null {
      return state.activeRuntimeSummary
    },
    runtimeStageSummaries(state): QueryRuntimeStageSummary[] {
      return state.activeRuntimeStageSummaries
    },
    activeBundleId(state): string | null {
      return state.activeExecution?.contextBundleId ?? state.activeBundle?.bundle.bundleId ?? null
    },
  },
  actions: {
    reset(): void {
      this.activeLibraryId = null
      this.sessions = []
      this.activeSession = null
      this.activeExecution = null
      this.activeRuntimeSummary = null
      this.activeRuntimeStageSummaries = []
      this.activeBundle = null
      this.loadingSessions = false
      this.loadingSession = false
      this.loadingExecution = false
      this.executingTurn = false
      this.error = null
      this.graphSurfacePriority = 'secondary'
    },
    setGraphSurfacePriority(value: QueryState['graphSurfacePriority']): void {
      this.graphSurfacePriority = value
    },
    async loadSessions(libraryId?: string): Promise<void> {
      const scope = resolveShellScope()
      const targetLibraryId = libraryId ?? scope?.libraryId ?? null
      if (!targetLibraryId) {
        this.reset()
        return
      }

      this.loadingSessions = true
      this.error = null
      this.activeLibraryId = targetLibraryId
      try {
        this.sessions = await listQuerySessions(targetLibraryId)
      } catch (error) {
        this.error = normalizeErrorMessage(error, 'Failed to load query sessions')
        throw error
      } finally {
        this.loadingSessions = false
      }
    },
    async createSession(payload?: Partial<CreateQuerySessionPayload>): Promise<QuerySession> {
      const scope = resolveShellScope()
      if (!scope) {
        const error = new Error('Select a workspace and library before starting a query session')
        this.error = error.message
        throw error
      }

      this.loadingSession = true
      this.error = null
      try {
        const session = await createQuerySession({
          workspaceId: payload?.workspaceId ?? scope.workspaceId,
          libraryId: payload?.libraryId ?? scope.libraryId,
          title: payload?.title ?? null,
        })
        this.activeLibraryId = session.libraryId
        await this.loadSession(session.id)
        await this.loadSessions(session.libraryId)
        return session
      } catch (error) {
        this.error = normalizeErrorMessage(error, 'Failed to create query session')
        throw error
      } finally {
        this.loadingSession = false
      }
    },
    async loadSession(sessionId: string): Promise<void> {
      this.loadingSession = true
      this.error = null
      try {
        const detail = await fetchQuerySessionDetail(sessionId)
        this.activeLibraryId = detail.session.libraryId
        this.activeSession = detail
        this.sessions = [
          detail.session,
          ...this.sessions.filter((item) => item.id !== detail.session.id),
        ]
      } catch (error) {
        this.error = normalizeErrorMessage(error, 'Failed to load query session')
        throw error
      } finally {
        this.loadingSession = false
      }
    },
    async runTurn(
      sessionId: string,
      payload: ExecuteQueryTurnPayload,
      handlers: ExecuteQueryTurnStreamHandlers = {},
    ): Promise<void> {
      this.executingTurn = true
      this.error = null
      this.activeRuntimeSummary = null
      this.activeRuntimeStageSummaries = []
      try {
        const result = await executeQueryTurn(sessionId, payload, {
          ...handlers,
          onRuntime: (runtime) => {
            this.activeRuntimeSummary = runtime
            handlers.onRuntime?.(runtime)
          },
        })
        this.activeLibraryId = result.session.libraryId
        this.activeRuntimeSummary = result.runtimeSummary
        this.activeRuntimeStageSummaries = result.runtimeStageSummaries
        await this.loadSession(result.session.id)
        await this.loadExecution(result.execution.id)
      } catch (error) {
        try {
          await this.loadSession(sessionId)
          const latestExecutionId = this.activeSession?.executions[0]?.id ?? null
          if (latestExecutionId) {
            await this.loadExecution(latestExecutionId)
          }
        } catch {
          // Preserve the original execution error; best-effort refresh only.
        }
        this.error = normalizeErrorMessage(error, 'Failed to execute grounded query turn')
        throw error
      } finally {
        this.executingTurn = false
      }
    },
    async loadExecution(executionId: string): Promise<void> {
      this.loadingExecution = true
      this.error = null
      try {
        const detail = await fetchQueryExecutionDetail(executionId)
        this.activeExecution = detail
        this.activeRuntimeSummary = detail.runtimeSummary
        this.activeRuntimeStageSummaries = detail.runtimeStageSummaries
        if (detail.contextBundleId) {
          try {
            this.activeBundle = await fetchKnowledgeContextBundle(detail.contextBundleId)
          } catch (error) {
            if (error instanceof ApiClientError && error.statusCode === 404) {
              this.activeBundle = null
            } else {
              throw error
            }
          }
        } else {
          this.activeBundle = null
        }
      } catch (error) {
        this.error = normalizeErrorMessage(error, 'Failed to load query execution')
        throw error
      } finally {
        this.loadingExecution = false
      }
    },
    async loadBundle(bundleId: string): Promise<void> {
      this.loadingExecution = true
      this.error = null
      try {
        this.activeBundle = await fetchKnowledgeContextBundle(bundleId)
      } catch (error) {
        this.error = normalizeErrorMessage(error, 'Failed to load grounded context bundle')
        throw error
      } finally {
        this.loadingExecution = false
      }
    },
  },
})
