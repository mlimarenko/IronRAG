<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute, useRouter } from 'vue-router'

import {
  fetchChatMessages,
  fetchChatSessions,
  fetchProjectReadiness,
  fetchRetrievalRunDetail,
  isUnauthorizedApiError,
  runQuery,
  type ChatMessageSurface,
  type ChatSessionSurface,
  type ProjectReadinessSummary,
  type QueryResponseSurface,
  type RetrievalRunDetail,
} from 'src/boot/api'
import ReferenceList from 'src/components/chat/ReferenceList.vue'
import RetrievalDiagnosticsPanel from 'src/components/chat/RetrievalDiagnosticsPanel.vue'
import StatusPill from 'src/components/chat/StatusPill.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import { formatLocaleDateTime } from 'src/lib/formatting'
import {
  fetchGraphEntityDetail,
  fetchGraphProductSnapshot,
  fetchGraphProjectDiagnostics,
  fetchGraphProjectSummary,
  fetchGraphSubgraph,
  isGraphApiUnavailableError,
  searchGraphProduct,
  type GraphEntityDetailResponse,
  type GraphEntitySummary,
  type GraphProjectDiagnosticsResponse,
  type GraphProjectSummaryResponse,
  type GraphRelationDetail,
  type GraphRelationSummary,
  type GraphSearchResponse,
  type GraphSubgraphResponse,
} from 'src/lib/graphProduct'
import { formatProjectReadiness } from 'src/lib/projectReadiness'
import { hydrateWorkspaceProjectScope } from 'src/lib/productFlow'
import { getSelectedProjectId, setSelectedProjectId } from 'src/stores/flow'
import { ensureProjectMatchesWorkspace } from 'src/lib/flowSelection'

interface WorkspaceItem {
  id: string
  slug: string
  name: string
}

interface ProjectItem {
  id: string
  slug: string
  name: string
  workspace_id: string
}

type BannerTone = 'warning' | 'info'

interface GraphResultCard {
  id: string
  kind: 'entity' | 'relation'
  title: string
  subtitle: string
  summary: string
  badge: string
  sourceChunkCount: number
  matchReasons: string[]
  entity?: GraphEntitySummary
  relation?: GraphRelationSummary
  fromEntityName?: string
  toEntityName?: string
}

interface GraphSelection {
  id: string
  kind: 'entity' | 'relation'
}

const MOBILE_BREAKPOINT = 900

const { t } = useI18n()
const route = useRoute()
const router = useRouter()

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const queryInputRef = ref<HTMLTextAreaElement | null>(null)
const timelineListRef = ref<HTMLElement | null>(null)

const queryText = ref('')
const result = ref<QueryResponseSurface | null>(null)
const detail = ref<RetrievalRunDetail | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)
const sessionLoading = ref(false)
const sessions = ref<ChatSessionSurface[]>([])
const messages = ref<ChatMessageSurface[]>([])
const activeSessionId = ref('')
const showMobileSessions = ref(false)
const windowWidth = ref(typeof window === 'undefined' ? MOBILE_BREAKPOINT + 1 : window.innerWidth)

const graphSearchQuery = ref('')
const subgraphDepth = ref(1)
const projectSummary = ref<GraphProjectSummaryResponse | null>(null)
const projectDiagnostics = ref<GraphProjectDiagnosticsResponse | null>(null)
const searchResponse = ref<GraphSearchResponse | null>(null)
const entityDetail = ref<GraphEntityDetailResponse | null>(null)
const entitySubgraph = ref<GraphSubgraphResponse | null>(null)
const loadingGraphSurface = ref(false)
const loadingGraphSearch = ref(false)
const loadingGraphDetail = ref(false)
const graphApiUnavailable = ref(false)
const graphSurfaceError = ref<string | null>(null)
const graphSearchError = ref<string | null>(null)
const graphDetailError = ref<string | null>(null)
const selectedGraphItem = ref<GraphSelection | null>(null)
const showGraphContext = ref(false)
const showTechnicalGraphDetail = ref(false)
const showGraphReadinessDetails = ref(false)

let graphSearchTimer: number | undefined
let graphSurfaceRequestId = 0
let graphSearchRequestId = 0
let graphDetailRequestId = 0

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const readinessPresentation = computed(() => formatProjectReadiness(readiness.value, t))
const hasIndexedDocuments = computed(() => readinessPresentation.value.hasAnyDocuments)
const hasIngestionRuns = computed(() => (readiness.value?.ingestion_jobs ?? 0) > 0)
const canSubmit = computed(() =>
  Boolean(
    selectedProjectId.value &&
    queryText.value.trim() &&
    !loading.value &&
    readinessPresentation.value.queryable,
  ),
)
const activeSession = computed(
  () => sessions.value.find((session) => session.id === activeSessionId.value) ?? null,
)
const timeline = computed(() => messages.value)
const hasTimeline = computed(() => timeline.value.length > 0)
const isMobile = computed(() => windowWidth.value <= MOBILE_BREAKPOINT)
const shouldShowDesktopSidebar = computed(() => !isMobile.value)
const shouldShowMobileSessionToggle = computed(() => isMobile.value && sessions.value.length > 0)
const shouldShowSessionList = computed(
  () => shouldShowDesktopSidebar.value || showMobileSessions.value,
)
const shouldShowTechnicalDetails = computed(() => Boolean(result.value && detail.value))
const shouldShowQuestionExamples = computed(() =>
  Boolean(
    selectedProject.value &&
    readiness.value?.ready_for_query &&
    !queryText.value.trim() &&
    !loading.value,
  ),
)
const questionExamples = computed(() => [
  t('flow.search.query.examples.summary'),
  t('flow.search.query.examples.risks'),
  t('flow.search.query.examples.next'),
])
const mobileSessionToggleLabel = computed(() => {
  if (showMobileSessions.value) {
    return t('flow.search.sessions.hideAction')
  }

  return activeSession.value
    ? t('flow.search.sessions.resumeAction', { id: activeSession.value.id.slice(0, 8) })
    : t('flow.search.sessions.showAction', { count: sessions.value.length })
})
const pageStatus = computed(() => {
  if (result.value) {
    return {
      status: result.value.answer_status,
      label: result.value.weak_grounding
        ? t('flow.search.statusWeak')
        : t('flow.search.statusReady'),
    }
  }

  if (!selectedProject.value) {
    return { status: 'blocked', label: t('flow.search.statusBlocked') }
  }

  if (activeSession.value) {
    return { status: 'draft', label: t('flow.search.statusDraft') }
  }

  if (readinessPresentation.value.queryable) {
    return {
      status: readinessPresentation.value.hasFailures ? 'partial' : 'draft',
      label: readinessPresentation.value.hasFailures
        ? t('flow.search.statusReadyWithWarnings')
        : t('flow.search.statusDraft'),
    }
  }

  if (hasIndexedDocuments.value || hasIngestionRuns.value) {
    return { status: 'partial', label: t('flow.search.statusIndexing') }
  }

  return { status: 'blocked', label: t('flow.search.statusNeedsContent') }
})
const queryHint = computed(() => {
  if (!selectedProject.value) {
    return t('flow.search.query.hintBlocked')
  }

  if (activeSession.value) {
    return t('flow.search.query.hintResume')
  }

  if (readinessPresentation.value.queryable) {
    return readinessPresentation.value.askHint
  }

  if (hasIndexedDocuments.value || hasIngestionRuns.value) {
    return t('flow.search.query.hintIndexing')
  }

  return t('flow.search.query.hintNoContent')
})
const composerStatus = computed(() => {
  if (!selectedProject.value) {
    return t('flow.search.query.statusBlocked')
  }

  if (loading.value) {
    return t('flow.search.query.statusBusy')
  }

  if (!readinessPresentation.value.queryable) {
    return hasIndexedDocuments.value || hasIngestionRuns.value
      ? t('flow.search.query.statusIndexing')
      : t('flow.search.query.statusNoContent')
  }

  return activeSession.value
    ? t('flow.search.query.statusResume', { count: timeline.value.length })
    : t('flow.search.query.statusReady')
})
const recentSessionsSummary = computed(() => {
  if (!sessions.value.length) {
    return t('flow.search.sessions.empty')
  }

  if (activeSession.value) {
    return t('flow.search.sessions.summaryActive', {
      count: sessions.value.length,
      title:
        activeSession.value.title ??
        t('flow.search.sessions.fallbackTitle', { id: activeSession.value.id.slice(0, 8) }),
    })
  }

  return t('flow.search.sessions.summary', { count: sessions.value.length })
})
const resultNotice = computed<{ tone: BannerTone; message: string } | null>(() => {
  if (!result.value) {
    return null
  }

  if (result.value.warning) {
    return {
      tone: 'warning',
      message: `${t('flow.search.result.warningDetail')}: ${result.value.warning}`,
    }
  }

  if (result.value.weak_grounding) {
    return { tone: 'warning', message: t('flow.search.result.warningWeak') }
  }

  if (!result.value.references.length) {
    return { tone: 'info', message: t('flow.search.result.warningNoReferences') }
  }

  return null
})
const readinessNotice = computed<{ tone: BannerTone; title: string; message: string } | null>(
  () => {
    if (!selectedProject.value) {
      return null
    }

    if (readinessPresentation.value.queryable && !readinessPresentation.value.freshnessHint) {
      return null
    }

    if (!hasIndexedDocuments.value && !hasIngestionRuns.value) {
      return {
        tone: 'info',
        title: t('flow.search.readiness.emptyState.title'),
        message: t('flow.search.readiness.emptyState.body'),
      }
    }

    if (readinessPresentation.value.queryable) {
      return {
        tone: 'warning',
        title: t('flow.search.readiness.warningState.title'),
        message: readinessPresentation.value.freshnessHint ?? readinessPresentation.value.askHint,
      }
    }

    return {
      tone: 'warning',
      title: t('flow.search.readiness.partialState.title'),
      message: t('flow.search.readiness.partialState.body', {
        state: formatIndexingStateLabel(readiness.value?.indexing_state),
        documents: readiness.value?.documents ?? 0,
        jobs: readiness.value?.ingestion_jobs ?? 0,
      }),
    }
  },
)

const currentGraphCoverage = computed(
  () => projectDiagnostics.value?.coverage ?? projectSummary.value?.coverage ?? null,
)
const graphCoverageWarning = computed(
  () => projectDiagnostics.value?.coverage.warning ?? currentGraphCoverage.value?.warning ?? null,
)
const graphReadinessSummary = computed(() => projectDiagnostics.value?.readiness ?? null)
const graphContentSummary = computed(() => projectDiagnostics.value?.content ?? null)
const graphProvenanceSummary = computed(() => projectDiagnostics.value?.provenance ?? null)
const graphDiagnosticsBlockers = computed(() => graphReadinessSummary.value?.blockers ?? [])
const graphDiagnosticsNextSteps = computed(() => graphReadinessSummary.value?.next_steps ?? [])
const graphEntityNameById = computed<Record<string, string>>(() => {
  if (!projectSummary.value) {
    return {}
  }

  return Object.fromEntries(
    projectSummary.value.top_entities.map((entity) => [entity.id, entity.canonical_name]),
  )
})
const graphDefaultResultCards = computed<GraphResultCard[]>(() => {
  if (!projectSummary.value) {
    return []
  }

  const entityCards = projectSummary.value.top_entities.map((entity) => ({
    id: entity.id,
    kind: 'entity' as const,
    title: entity.canonical_name,
    subtitle: t('flow.search.graph.labels.entity'),
    summary: t('flow.search.graph.entitySummary', {
      count: formatCount(entity.source_chunk_count, 'supporting passage'),
    }),
    badge: t('flow.search.graph.labels.entity'),
    sourceChunkCount: entity.source_chunk_count,
    matchReasons: [],
    entity,
  }))

  const relationCards = projectSummary.value.sample_relations.map((relation) =>
    relationToCard(relation, graphEntityNameById.value, []),
  )

  return [...entityCards, ...relationCards]
})
const graphSearchResultCards = computed<GraphResultCard[]>(() => {
  if (!searchResponse.value) {
    return []
  }

  const entityCards = searchResponse.value.entity_results.map(({ entity, match_reasons }) => ({
    id: entity.id,
    kind: 'entity' as const,
    title: entity.canonical_name,
    subtitle: entity.entity_type ?? t('flow.search.graph.labels.entity'),
    summary: formatReasonsSummary(match_reasons, entity.source_chunk_count),
    badge: t('flow.search.graph.labels.entity'),
    sourceChunkCount: entity.source_chunk_count,
    matchReasons: match_reasons,
    entity,
  }))

  const relationCards = searchResponse.value.relation_results.map(
    ({ relation, from_entity_name, to_entity_name, match_reasons }) =>
      relationToCard(
        relation,
        graphEntityNameById.value,
        match_reasons,
        from_entity_name,
        to_entity_name,
      ),
  )

  return [...entityCards, ...relationCards]
})
const visibleGraphResults = computed<GraphResultCard[]>(() =>
  graphSearchQuery.value.trim() ? graphSearchResultCards.value : graphDefaultResultCards.value,
)
const selectedGraphCard = computed<GraphResultCard | null>(() => {
  const selectedItem = selectedGraphItem.value

  if (!selectedItem) {
    return visibleGraphResults.value[0] ?? null
  }

  return (
    visibleGraphResults.value.find(
      (item) => item.kind === selectedItem.kind && item.id === selectedItem.id,
    ) ?? null
  )
})
const selectedGraphEntityCard = computed(() =>
  selectedGraphCard.value?.kind === 'entity' ? selectedGraphCard.value : null,
)
const selectedGraphRelationCard = computed(() =>
  selectedGraphCard.value?.kind === 'relation' ? selectedGraphCard.value : null,
)
const canLoadGraphSubgraph = computed(() =>
  Boolean(
    selectedProjectId.value &&
    selectedGraphCard.value?.kind === 'entity' &&
    !graphApiUnavailable.value,
  ),
)
const selectedSubgraphEntityName = computed(() =>
  selectedGraphCard.value?.kind === 'entity' ? selectedGraphCard.value.title : '',
)
const graphPanelStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: t('flow.search.graph.status.noProject') }
  }

  if (loadingGraphSurface.value) {
    return { status: 'pending', label: t('flow.search.graph.status.loading') }
  }

  if (graphApiUnavailable.value) {
    return { status: 'blocked', label: t('flow.search.graph.status.unavailable') }
  }

  if (graphSurfaceError.value) {
    return { status: 'warning', label: t('flow.search.graph.status.degraded') }
  }

  if ((currentGraphCoverage.value?.relation_count ?? 0) > 0) {
    return { status: 'ready', label: t('flow.search.graph.status.live') }
  }

  if ((currentGraphCoverage.value?.entity_count ?? 0) > 0) {
    return { status: 'partial', label: t('flow.search.graph.status.entityOnly') }
  }

  return { status: 'draft', label: t('flow.search.graph.status.waiting') }
})

watch(
  () => route.query.q,
  (value) => {
    if (typeof value === 'string' && value.trim()) {
      queryText.value = value.trim()
    }
  },
  { immediate: true },
)

watch(
  () => route.query.session,
  async (value) => {
    if (typeof value !== 'string' || !value.trim()) {
      return
    }

    const nextSessionId = value.trim()
    if (
      !sessions.value.some((session) => session.id === nextSessionId) ||
      activeSessionId.value === nextSessionId
    ) {
      return
    }

    await reopenSession(nextSessionId)
  },
  { immediate: true },
)

watch(
  () => selectedProjectId.value,
  async (projectId) => {
    const scopedProjectId = ensureProjectMatchesWorkspace(projects.value, projectId)
    result.value = null
    detail.value = null
    errorMessage.value = null
    readiness.value = null
    sessions.value = []
    messages.value = []
    activeSessionId.value = ''
    showMobileSessions.value = false
    resetGraphSurface()

    if (!scopedProjectId) {
      return
    }

    if (scopedProjectId !== projectId) {
      setSelectedProjectId(scopedProjectId)
    }

    await Promise.all([
      loadReadiness(scopedProjectId),
      loadSessions(scopedProjectId),
      loadGraphSurface(scopedProjectId),
    ])
  },
)

watch(activeSessionId, async (sessionId, previousSessionId) => {
  if (!sessionId || sessionId === previousSessionId) {
    return
  }

  await syncRouteSession(sessionId)
})

watch(
  () => timeline.value.length,
  async (nextCount, previousCount) => {
    if (!nextCount || nextCount === previousCount) {
      return
    }

    await scrollTimelineToLatest()
  },
)

watch(isMobile, (mobile) => {
  if (!mobile) {
    showMobileSessions.value = false
  }
})

watch(
  visibleGraphResults,
  (items) => {
    if (!items.length) {
      selectedGraphItem.value = null
      entityDetail.value = null
      entitySubgraph.value = null
      graphDetailError.value = null
      return
    }

    const selectedItem = selectedGraphItem.value

    if (
      selectedItem &&
      items.some((item) => item.id === selectedItem.id && item.kind === selectedItem.kind)
    ) {
      return
    }

    selectedGraphItem.value = { id: items[0].id, kind: items[0].kind }
  },
  { immediate: true },
)

watch([selectedGraphItem, subgraphDepth], ([item]) => {
  graphDetailRequestId += 1
  entityDetail.value = null
  entitySubgraph.value = null
  graphDetailError.value = null
  loadingGraphDetail.value = false

  if (item?.kind !== 'entity' || !selectedProjectId.value || graphApiUnavailable.value) {
    return
  }

  const requestId = graphDetailRequestId
  loadingGraphDetail.value = true

  void Promise.all([
    fetchGraphEntityDetail(selectedProjectId.value, item.id),
    fetchGraphSubgraph(selectedProjectId.value, item.id, subgraphDepth.value),
  ])
    .then(([detailResponse, subgraph]) => {
      if (requestId !== graphDetailRequestId) {
        return
      }

      entityDetail.value = detailResponse
      entitySubgraph.value = subgraph
    })
    .catch((error: unknown) => {
      if (requestId !== graphDetailRequestId) {
        return
      }

      graphDetailError.value =
        error instanceof Error ? error.message : t('flow.search.graph.errors.detail')
    })
    .finally(() => {
      if (requestId === graphDetailRequestId) {
        loadingGraphDetail.value = false
      }
    })
})

watch(graphSearchQuery, (value) => {
  graphSearchError.value = null
  searchResponse.value = null

  if (graphSearchTimer) {
    window.clearTimeout(graphSearchTimer)
  }

  const query = value.trim()
  if (!query || !selectedProjectId.value || graphApiUnavailable.value) {
    loadingGraphSearch.value = false
    return
  }

  const requestId = ++graphSearchRequestId
  loadingGraphSearch.value = true

  graphSearchTimer = window.setTimeout(() => {
    void searchGraphProduct(selectedProjectId.value, query)
      .then((response) => {
        if (requestId !== graphSearchRequestId) {
          return
        }

        searchResponse.value = response
      })
      .catch((error: unknown) => {
        if (requestId !== graphSearchRequestId) {
          return
        }

        graphSearchError.value =
          error instanceof Error ? error.message : t('flow.search.graph.errors.search')
      })
      .finally(() => {
        if (requestId === graphSearchRequestId) {
          loadingGraphSearch.value = false
        }
      })
  }, 250)
})

onMounted(async () => {
  updateViewportWidth()
  window.addEventListener('resize', updateViewportWidth, { passive: true })

  const scope = await hydrateWorkspaceProjectScope({
    setWorkspaces: (items) => {
      workspaces.value = items
    },
    setProjects: (items) => {
      projects.value = items
    },
  })

  if (scope.projectId) {
    await Promise.all([
      loadReadiness(scope.projectId),
      loadSessions(scope.projectId),
      loadGraphSurface(scope.projectId),
    ])
  }
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', updateViewportWidth)
  if (graphSearchTimer) {
    window.clearTimeout(graphSearchTimer)
  }
})

function updateViewportWidth() {
  windowWidth.value = window.innerWidth
}

function formatIndexingStateLabel(state?: string | null) {
  const normalized = state?.trim().toLowerCase() ?? ''

  switch (normalized) {
    case 'ready':
    case 'query_ready':
      return t('flow.search.readiness.states.ready')
    case 'indexing':
    case 'processing':
    case 'partial':
      return t('flow.search.readiness.states.indexing')
    case 'empty':
    case 'pending':
    case 'not_ready':
      return t('flow.search.readiness.states.empty')
    default:
      return state?.trim() ?? t('flow.search.readiness.states.unknown')
  }
}

function formatMessageTimestamp(value: string) {
  return formatLocaleDateTime(value) ?? value
}

async function loadReadiness(projectId: string) {
  try {
    readiness.value = await fetchProjectReadiness(projectId)
  } catch {
    readiness.value = null
  }
}

async function loadSessions(projectId: string) {
  sessionLoading.value = true
  try {
    sessions.value = await fetchChatSessions(projectId)

    const requestedSessionId =
      typeof route.query.session === 'string' && route.query.session.trim()
        ? route.query.session.trim()
        : ''
    const requestedSessionExists = requestedSessionId
      ? sessions.value.some((session) => session.id === requestedSessionId)
      : false

    if (requestedSessionExists) {
      activeSessionId.value = requestedSessionId
      await loadMessages(requestedSessionId)
      return
    }

    if (
      activeSessionId.value &&
      sessions.value.some((session) => session.id === activeSessionId.value)
    ) {
      await loadMessages(activeSessionId.value)
      return
    }

    const nextSessionId = sessions.value[0]?.id ?? ''
    activeSessionId.value = nextSessionId
    if (nextSessionId) {
      await loadMessages(nextSessionId)
    } else {
      messages.value = []
      await syncRouteSession('')
    }
  } catch {
    sessions.value = []
    messages.value = []
    activeSessionId.value = ''
    await syncRouteSession('')
  } finally {
    sessionLoading.value = false
  }
}

async function loadMessages(sessionId: string) {
  activeSessionId.value = sessionId
  try {
    messages.value = await fetchChatMessages(sessionId)
    await scrollTimelineToLatest()
  } catch {
    messages.value = []
  }
}

function applyQuestionExample(example: string) {
  queryText.value = example
  void nextTick(() => {
    queryInputRef.value?.focus()
    queryInputRef.value?.setSelectionRange(example.length, example.length)
  })
}

function handleTextareaKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || event.shiftKey || !(event.metaKey || event.ctrlKey)) {
    return
  }

  event.preventDefault()
  if (canSubmit.value) {
    void submitQuery()
  }
}

async function reopenSession(sessionId: string) {
  result.value = null
  detail.value = null
  errorMessage.value = null
  showMobileSessions.value = false
  await loadMessages(sessionId)
}

function focusComposer() {
  queryInputRef.value?.focus()
}

function toggleMobileSessions() {
  showMobileSessions.value = !showMobileSessions.value
}

async function syncRouteSession(sessionId: string) {
  const currentSession = typeof route.query.session === 'string' ? route.query.session : ''
  const nextSession = sessionId || undefined

  if (currentSession === (nextSession ?? '')) {
    return
  }

  const nextQuery = { ...route.query }
  if (nextSession) {
    nextQuery.session = nextSession
  } else {
    delete nextQuery.session
  }

  await router.replace({ query: nextQuery })
}

async function scrollTimelineToLatest() {
  await nextTick()
  const element = timelineListRef.value
  if (!element) {
    return
  }

  element.scrollTo({ top: element.scrollHeight, behavior: 'smooth' })
}

async function submitQuery() {
  const trimmedQuery = queryText.value.trim()
  if (!trimmedQuery) {
    return
  }

  loading.value = true
  errorMessage.value = null
  result.value = null
  detail.value = null

  try {
    if (!selectedProjectId.value) {
      throw new Error(t('flow.search.query.hintBlocked'))
    }

    if (!readinessPresentation.value.queryable) {
      throw new Error(
        hasIndexedDocuments.value || hasIngestionRuns.value
          ? t('flow.search.query.hintIndexing')
          : t('flow.search.query.hintNoContent'),
      )
    }

    const response = await runQuery({
      project_id: selectedProjectId.value,
      session_id: activeSessionId.value || undefined,
      query_text: trimmedQuery,
      top_k: 8,
    })

    result.value = response
    queryText.value = ''
    activeSessionId.value = response.session_id
    showMobileSessions.value = false

    await Promise.all([
      loadSessions(selectedProjectId.value),
      loadMessages(response.session_id),
      loadGraphSurface(selectedProjectId.value),
    ])

    try {
      detail.value = await fetchRetrievalRunDetail(response.retrieval_run_id)
    } catch {
      detail.value = null
    }

    focusComposer()
  } catch (error) {
    errorMessage.value = isUnauthorizedApiError(error)
      ? t('flow.search.authRequired')
      : error instanceof Error
        ? error.message
        : t('flow.search.error')
  } finally {
    loading.value = false
  }
}

function resetGraphSurface() {
  graphSearchQuery.value = ''
  projectSummary.value = null
  projectDiagnostics.value = null
  searchResponse.value = null
  entityDetail.value = null
  entitySubgraph.value = null
  loadingGraphSurface.value = false
  loadingGraphSearch.value = false
  loadingGraphDetail.value = false
  graphApiUnavailable.value = false
  graphSurfaceError.value = null
  graphSearchError.value = null
  graphDetailError.value = null
  selectedGraphItem.value = null
  subgraphDepth.value = 1
}

async function loadGraphSurface(projectId: string) {
  graphSurfaceRequestId += 1
  const requestId = graphSurfaceRequestId

  graphSearchQuery.value = ''
  searchResponse.value = null
  entityDetail.value = null
  entitySubgraph.value = null
  selectedGraphItem.value = null
  graphApiUnavailable.value = false
  graphSurfaceError.value = null
  graphSearchError.value = null
  graphDetailError.value = null
  projectSummary.value = null
  projectDiagnostics.value = null
  subgraphDepth.value = 1

  if (!projectId) {
    loadingGraphSurface.value = false
    return
  }

  loadingGraphSurface.value = true

  try {
    const [, summary, diagnostics] = await Promise.all([
      fetchGraphProductSnapshot(projectId),
      fetchGraphProjectSummary(projectId),
      fetchGraphProjectDiagnostics(projectId),
    ])

    if (requestId !== graphSurfaceRequestId) {
      return
    }

    projectSummary.value = summary
    projectDiagnostics.value = diagnostics
  } catch (error) {
    if (requestId !== graphSurfaceRequestId) {
      return
    }

    if (isGraphApiUnavailableError(error)) {
      graphApiUnavailable.value = true
      graphSurfaceError.value = null
    } else {
      graphSurfaceError.value =
        error instanceof Error ? error.message : t('flow.search.graph.errors.load')
    }
  } finally {
    if (requestId === graphSurfaceRequestId) {
      loadingGraphSurface.value = false
    }
  }
}

function selectGraphCard(item: GraphResultCard) {
  selectedGraphItem.value = { id: item.id, kind: item.kind }
}

function handleSubgraphDepthChange(event: Event) {
  const target = event.target
  if (!(target instanceof HTMLSelectElement)) {
    return
  }

  subgraphDepth.value = Number.parseInt(target.value, 10) || 1
}

function relationToCard(
  relation: GraphRelationSummary,
  entityNames: Partial<Record<string, string>>,
  matchReasons: string[],
  fromEntityName?: string,
  toEntityName?: string,
): GraphResultCard {
  const fromName = fromEntityName ?? entityNames[relation.from_entity_id] ?? relation.from_entity_id
  const toName = toEntityName ?? entityNames[relation.to_entity_id] ?? relation.to_entity_id

  return {
    id: relation.id,
    kind: 'relation',
    title: `${fromName} ${relation.relation_type} ${toName}`,
    subtitle: 'Relation',
    summary: formatReasonsSummary(matchReasons, relation.source_chunk_count),
    badge: relation.relation_type,
    sourceChunkCount: relation.source_chunk_count,
    matchReasons,
    relation,
    fromEntityName: fromName,
    toEntityName: toName,
  }
}

function formatCount(value: number, singular: string): string {
  const plural = singular.endsWith('s') ? singular : `${singular}s`
  return `${String(value)} ${value === 1 ? singular : plural}`
}

function formatReasonsSummary(reasons: string[], chunkCount: number): string {
  if (!reasons.length) {
    return `${formatCount(chunkCount, 'supporting chunk')} linked to this record.`
  }

  return `Matched on ${reasons.join(', ')}. ${formatCount(chunkCount, 'supporting chunk')} linked to this record.`
}

function formatRelationLine(relation: GraphRelationDetail): string {
  return `${relation.from_entity_name} ${relation.relation.relation_type} ${relation.to_entity_name}`
}
</script>

<template>
  <section class="rr-page-grid chat-page" :class="{ 'chat-page--mobile': isMobile }">
    <PageSection
      :title="t('flow.search.title')"
      :description="isMobile ? t('flow.search.descriptionMobile') : t('flow.search.description')"
      :status="pageStatus.status"
      :status-label="pageStatus.label"
      :compact-header="isMobile"
      :hide-actions="isMobile"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/documents">
          {{ t('flow.search.action') }}
        </RouterLink>
      </template>

      <div class="chat-page__layout" :class="{ 'chat-page__layout--single': isMobile }">
        <div class="chat-page__main">
          <div class="chat-page__conversation">
            <details
              v-if="shouldShowSessionList"
              class="rr-panel rr-panel--muted session-sidebar"
              :open="showMobileSessions || !isMobile"
            >
              <summary class="session-sidebar__summary">
                <div>
                  <p class="rr-kicker">{{ t('flow.search.sessions.kicker') }}</p>
                  <h3>{{ t('flow.search.sessions.title') }}</h3>
                </div>
                <p class="rr-note">{{ recentSessionsSummary }}</p>
              </summary>

              <div class="session-sidebar__body">
                <p class="rr-note">
                  {{
                    isMobile
                      ? t('flow.search.sessions.mobileDescription')
                      : t('flow.search.sessions.description')
                  }}
                </p>
                <div v-if="sessionLoading" class="rr-note">
                  {{ t('flow.search.sessions.loading') }}
                </div>
                <div v-else-if="!sessions.length" class="rr-note">
                  {{ t('flow.search.sessions.empty') }}
                </div>
                <button
                  v-for="session in sessions"
                  :key="session.id"
                  type="button"
                  class="session-item"
                  :data-active="session.id === activeSessionId"
                  @click="reopenSession(session.id)"
                >
                  <strong>
                    {{
                      session.title ||
                      t('flow.search.sessions.fallbackTitle', { id: session.id.slice(0, 8) })
                    }}
                  </strong>
                  <span>
                    {{ session.last_message_preview || t('flow.search.sessions.emptyPreview') }}
                  </span>
                  <small>
                    {{ t('flow.search.sessions.count', { count: session.message_count }) }}
                  </small>
                </button>
              </div>
            </details>

            <div
              v-if="shouldShowMobileSessionToggle"
              class="mobile-session-bar rr-panel rr-panel--muted"
            >
              <div class="mobile-session-bar__copy">
                <p class="rr-kicker">{{ t('flow.search.sessions.mobileKicker') }}</p>
                <strong>
                  {{
                    activeSession
                      ? activeSession.title || t('flow.search.sessions.fallbackTitle')
                      : t('flow.search.timeline.current')
                  }}
                </strong>
                <p class="rr-note">
                  {{ t('flow.search.sessions.mobileHint', { count: sessions.length }) }}
                </p>
              </div>
              <button
                type="button"
                class="rr-button rr-button--secondary"
                @click="toggleMobileSessions"
              >
                {{ mobileSessionToggleLabel }}
              </button>
            </div>

            <article v-if="hasTimeline" class="rr-panel rr-panel--muted timeline-panel">
              <div class="timeline-panel__header">
                <div>
                  <p class="rr-kicker">{{ t('flow.search.timeline.kicker') }}</p>
                  <h3>
                    {{
                      activeSession
                        ? activeSession.title || t('flow.search.sessions.fallbackTitle')
                        : t('flow.search.timeline.current')
                    }}
                  </h3>
                </div>
                <span class="rr-note">{{
                  t('flow.search.timeline.count', { count: timeline.length })
                }}</span>
              </div>

              <div ref="timelineListRef" class="timeline-list">
                <article
                  v-for="message in timeline"
                  :key="message.id"
                  class="timeline-item"
                  :data-role="message.role"
                >
                  <div class="timeline-item__meta">
                    <strong>{{
                      message.role === 'assistant'
                        ? t('flow.search.timeline.assistant')
                        : t('flow.search.timeline.you')
                    }}</strong>
                    <span>{{ formatMessageTimestamp(message.created_at) }}</span>
                  </div>
                  <p>{{ message.content }}</p>
                </article>
              </div>
            </article>

            <p v-if="errorMessage" class="rr-banner" data-tone="danger">
              {{ errorMessage }}
            </p>

            <article v-if="result" class="rr-panel answer-panel">
              <div class="answer-panel__header">
                <div class="answer-panel__copy">
                  <p class="rr-kicker">{{ t('flow.search.result.kicker') }}</p>
                  <h3>{{ t('flow.search.result.title') }}</h3>
                </div>
                <StatusPill :status="result.answer_status" />
              </div>

              <p v-if="resultNotice" class="rr-banner" :data-tone="resultNotice.tone">
                {{ resultNotice.message }}
              </p>

              <div class="answer-body">
                <p class="answer-body__label">{{ t('flow.search.result.answerLabel') }}</p>
                <p class="answer-copy">{{ result.answer }}</p>
              </div>

              <ReferenceList
                :title="t('flow.search.result.referencesTitle')"
                :description="t('flow.search.result.referencesDescription')"
                :empty-message="t('flow.search.result.referencesEmpty')"
                :references="result.references"
              />
            </article>

            <RetrievalDiagnosticsPanel
              v-if="shouldShowTechnicalDetails && detail"
              :detail="detail"
            />

            <EmptyStateCard
              v-else-if="!selectedProject"
              :title="t('flow.search.empty.noProject.title')"
              :message="t('flow.search.empty.noProject.body')"
              :hint="t('flow.search.empty.noProject.hint')"
            >
              <template #actions>
                <RouterLink class="rr-button rr-button--secondary" to="/advanced/context">
                  {{ t('flow.search.empty.noProject.action') }}
                </RouterLink>
              </template>
            </EmptyStateCard>

            <EmptyStateCard
              v-else-if="!readinessPresentation.queryable"
              :title="
                hasIndexedDocuments || hasIngestionRuns
                  ? t('flow.search.empty.partial.title')
                  : t('flow.search.empty.noContent.title')
              "
              :message="
                hasIndexedDocuments || hasIngestionRuns
                  ? t('flow.search.empty.partial.body', {
                      state: formatIndexingStateLabel(readiness?.indexing_state),
                      documents: readiness?.documents ?? 0,
                      jobs: readiness?.ingestion_jobs ?? 0,
                    })
                  : t('flow.search.empty.noContent.body')
              "
              :hint="
                hasIndexedDocuments || hasIngestionRuns
                  ? t('flow.search.empty.partial.hint')
                  : t('flow.search.empty.noContent.hint')
              "
            >
              <template #actions>
                <RouterLink class="rr-button rr-button--secondary" to="/documents">
                  {{ t('flow.search.nextActions.openFiles') }}
                </RouterLink>
              </template>
            </EmptyStateCard>

            <article v-else class="rr-panel rr-panel--muted empty-answer">
              <p class="rr-kicker">{{ t('flow.search.result.waitingKicker') }}</p>
              <h3>{{ t('flow.search.result.waitingTitle') }}</h3>
              <p class="rr-note">{{ t('flow.search.result.waitingBody') }}</p>
            </article>
          </div>

          <article class="rr-panel rr-panel--accent ask-panel ask-composer" :data-sticky="isMobile">
            <div class="ask-panel__header">
              <div class="ask-panel__copy">
                <p class="rr-kicker">{{ t('flow.search.query.kicker') }}</p>
                <h2>{{ t('flow.search.query.title') }}</h2>
                <p class="rr-note">{{ queryHint }}</p>
              </div>

              <RouterLink class="rr-button rr-button--ghost" to="/documents">
                {{ t('flow.search.action') }}
              </RouterLink>
            </div>

            <p v-if="readinessNotice" class="rr-banner" :data-tone="readinessNotice.tone">
              <strong>{{ readinessNotice.title }}</strong>
              <span>{{ readinessNotice.message }}</span>
            </p>

            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.search.query.question') }}</span>
              <textarea
                ref="queryInputRef"
                v-model="queryText"
                class="rr-control ask-panel__input"
                :rows="isMobile ? 3 : 4"
                :placeholder="t('flow.search.query.placeholder')"
                :disabled="!selectedProject"
                @keydown="handleTextareaKeydown"
              />
            </label>

            <div v-if="shouldShowQuestionExamples" class="ask-panel__examples">
              <p class="rr-note ask-panel__examples-label">
                {{ t('flow.search.query.examplesLabel') }}
              </p>
              <div class="ask-panel__examples-list">
                <button
                  v-for="example in questionExamples"
                  :key="example"
                  type="button"
                  class="rr-button rr-button--ghost ask-panel__example"
                  @click="applyQuestionExample(example)"
                >
                  {{ example }}
                </button>
              </div>
            </div>

            <div class="rr-action-row ask-panel__actions">
              <button
                type="button"
                class="rr-button ask-panel__submit"
                :disabled="!canSubmit"
                @click="submitQuery"
              >
                {{ loading ? t('flow.search.query.actionBusy') : t('flow.search.query.action') }}
              </button>
              <div class="ask-panel__meta">
                <p class="rr-note">{{ t('flow.search.query.shortcut') }}</p>
                <p class="rr-note ask-panel__status">{{ composerStatus }}</p>
              </div>
            </div>
          </article>

          <details class="rr-panel rr-panel--muted graph-context-panel" :open="showGraphContext">
            <summary
              class="graph-context-panel__summary"
              @click.prevent="showGraphContext = !showGraphContext"
            >
              <div>
                <p class="rr-kicker">{{ t('flow.search.graph.kicker') }}</p>
                <h3>{{ t('flow.search.graph.title') }}</h3>
                <p class="rr-note">{{ t('flow.search.graph.description') }}</p>
              </div>
              <div class="graph-context-panel__summary-meta">
                <StatusBadge :status="graphPanelStatus.status" :label="graphPanelStatus.label" />
                <small class="rr-note">{{ t('flow.search.graph.summaryCta') }}</small>
              </div>
            </summary>

            <p v-if="graphCoverageWarning" class="rr-banner" data-tone="warning">
              {{ graphCoverageWarning }}
            </p>
            <p v-if="graphSurfaceError" class="rr-banner" data-tone="danger">
              {{ graphSurfaceError }}
            </p>

            <LoadingSkeletonPanel
              v-if="loadingGraphSurface"
              :title="t('flow.search.graph.loading')"
              :lines="4"
            />

            <EmptyStateCard
              v-else-if="!selectedProject"
              :title="t('flow.search.graph.empty.noProject.title')"
              :message="t('flow.search.graph.empty.noProject.body')"
              :hint="t('flow.search.graph.empty.noProject.hint')"
            />

            <EmptyStateCard
              v-else-if="graphApiUnavailable"
              :title="t('flow.search.graph.empty.unavailable.title')"
              :message="t('flow.search.graph.empty.unavailable.body')"
              :hint="t('flow.search.graph.empty.unavailable.hint')"
            />

            <div v-else class="graph-context-panel__body">
              <div class="graph-context-panel__metrics">
                <article class="metric-card">
                  <span class="metric-card__label">{{
                    t('flow.search.graph.metrics.entities')
                  }}</span>
                  <strong>{{
                    currentGraphCoverage
                      ? formatCount(currentGraphCoverage.entity_count, 'entity')
                      : '—'
                  }}</strong>
                </article>
                <article class="metric-card">
                  <span class="metric-card__label">{{
                    t('flow.search.graph.metrics.relations')
                  }}</span>
                  <strong>{{
                    currentGraphCoverage
                      ? formatCount(currentGraphCoverage.relation_count, 'relation')
                      : '—'
                  }}</strong>
                </article>
                <article class="metric-card">
                  <span class="metric-card__label">{{ t('flow.search.graph.metrics.runs') }}</span>
                  <strong>{{
                    currentGraphCoverage
                      ? formatCount(currentGraphCoverage.extraction_runs, 'run')
                      : '—'
                  }}</strong>
                </article>
              </div>

              <div class="graph-context-panel__search">
                <label class="rr-field">
                  <span class="rr-field__label">{{ t('flow.search.graph.search.label') }}</span>
                  <input
                    v-model="graphSearchQuery"
                    class="rr-control"
                    type="text"
                    :disabled="!selectedProjectId || graphApiUnavailable"
                    :placeholder="t('flow.search.graph.search.placeholder')"
                  />
                </label>
                <p v-if="loadingGraphSearch" class="rr-note">
                  {{ t('flow.search.graph.search.loading') }}
                </p>
                <p v-if="graphSearchError" class="rr-banner" data-tone="danger">
                  {{ graphSearchError }}
                </p>
              </div>
              <div class="graph-results">
                <h4>{{ t('flow.search.graph.search.resultsTitle') }}</h4>
                <div v-if="visibleGraphResults.length" class="graph-results__list">
                  <button
                    v-for="item in visibleGraphResults"
                    :key="`${item.kind}-${item.id}`"
                    type="button"
                    class="graph-result"
                    :data-active="
                      selectedGraphCard?.id === item.id && selectedGraphCard?.kind === item.kind
                    "
                    @click="selectGraphCard(item)"
                  >
                    <div class="graph-result__meta">
                      <span class="graph-result__kind">{{ item.subtitle }}</span>
                      <strong>{{ item.title }}</strong>
                    </div>
                    <StatusBadge :label="item.badge" />
                    <p>{{ item.summary }}</p>
                  </button>
                </div>
                <EmptyStateCard
                  v-else
                  :title="
                    graphSearchQuery.trim()
                      ? t('flow.search.graph.empty.noMatches.title')
                      : t('flow.search.graph.empty.noRows.title')
                  "
                  :message="
                    graphSearchQuery.trim()
                      ? t('flow.search.graph.empty.noMatches.body')
                      : t('flow.search.graph.empty.noRows.body')
                  "
                  :hint="
                    graphSearchQuery.trim()
                      ? t('flow.search.graph.empty.noMatches.hint')
                      : t('flow.search.graph.empty.noRows.hint')
                  "
                />
              </div>

              <div class="graph-detail">
                <div class="graph-detail__header">
                  <div>
                    <h4>{{ t('flow.search.graph.detail.title') }}</h4>
                    <p class="rr-note">{{ t('flow.search.graph.detail.description') }}</p>
                  </div>
                  <details class="technical-details" :open="showTechnicalGraphDetail">
                    <summary @click.prevent="showTechnicalGraphDetail = !showTechnicalGraphDetail">
                      <span>{{ t('flow.search.graph.detail.technicalSummary') }}</span>
                      <small>{{ t('flow.search.graph.detail.technicalHint') }}</small>
                    </summary>
                    <label class="subgraph-depth-field">
                      <span class="rr-field__label">{{
                        t('flow.search.graph.detail.subgraphDepth')
                      }}</span>
                      <select
                        class="rr-control"
                        :value="String(subgraphDepth)"
                        :disabled="!canLoadGraphSubgraph"
                        @change="handleSubgraphDepthChange"
                      >
                        <option value="1">1 hop</option>
                        <option value="2">2 hops</option>
                        <option value="3">3 hops</option>
                      </select>
                    </label>
                  </details>
                </div>

                <LoadingSkeletonPanel
                  v-if="loadingGraphDetail"
                  :title="t('flow.search.graph.detail.loading')"
                  :lines="5"
                />

                <template v-else-if="selectedGraphEntityCard && entityDetail">
                  <div class="graph-detail__grid">
                    <article class="detail-card">
                      <p class="rr-kicker">{{ selectedGraphEntityCard.subtitle }}</p>
                      <h4>{{ entityDetail.entity.canonical_name }}</h4>
                      <p class="rr-note">
                        {{
                          t('flow.search.graph.detail.entitySummary', {
                            count: entityDetail.observed_relation_count,
                          })
                        }}
                      </p>

                      <div class="token-section">
                        <span class="token-section__label">{{
                          t('flow.search.graph.detail.aliases')
                        }}</span>
                        <div v-if="entityDetail.aliases.length" class="token-list">
                          <span
                            v-for="alias in entityDetail.aliases"
                            :key="alias"
                            class="token-chip"
                            >{{ alias }}</span
                          >
                        </div>
                        <p v-else class="rr-note">{{ t('flow.search.graph.detail.noAliases') }}</p>
                      </div>

                      <div class="token-section">
                        <span class="token-section__label">{{
                          t('flow.search.graph.detail.documents')
                        }}</span>
                        <div v-if="entityDetail.source_document_ids.length" class="token-list">
                          <span
                            v-for="documentId in entityDetail.source_document_ids"
                            :key="documentId"
                            class="token-chip token-chip--mono"
                            >{{ documentId }}</span
                          >
                        </div>
                        <p v-else class="rr-note">
                          {{ t('flow.search.graph.detail.noDocuments') }}
                        </p>
                      </div>
                    </article>

                    <article class="detail-card">
                      <p class="rr-kicker">{{ t('flow.search.graph.detail.subgraphEyebrow') }}</p>
                      <h4>
                        {{
                          t('flow.search.graph.detail.subgraphTitle', {
                            name: selectedSubgraphEntityName || entityDetail.entity.canonical_name,
                          })
                        }}
                      </h4>
                      <p class="rr-note">
                        {{
                          t('flow.search.graph.detail.subgraphStats', {
                            entities: entitySubgraph?.entity_count ?? 0,
                            relations: entitySubgraph?.relation_count ?? 0,
                          })
                        }}
                      </p>

                      <div class="token-section">
                        <span class="token-section__label">{{
                          t('flow.search.graph.detail.outgoingRelations')
                        }}</span>
                        <ul
                          v-if="entityDetail.outgoing_relations.length"
                          class="bullet-list bullet-list--compact"
                        >
                          <li
                            v-for="relation in entityDetail.outgoing_relations"
                            :key="relation.relation.id"
                          >
                            {{ formatRelationLine(relation) }}
                          </li>
                        </ul>
                        <p v-else class="rr-note">
                          {{ t('flow.search.graph.detail.noOutgoingRelations') }}
                        </p>
                      </div>

                      <div class="token-section">
                        <span class="token-section__label">{{
                          t('flow.search.graph.detail.incomingRelations')
                        }}</span>
                        <ul
                          v-if="entityDetail.incoming_relations.length"
                          class="bullet-list bullet-list--compact"
                        >
                          <li
                            v-for="relation in entityDetail.incoming_relations"
                            :key="relation.relation.id"
                          >
                            {{ formatRelationLine(relation) }}
                          </li>
                        </ul>
                        <p v-else class="rr-note">
                          {{ t('flow.search.graph.detail.noIncomingRelations') }}
                        </p>
                      </div>
                    </article>
                  </div>

                  <p v-if="entityDetail.warning" class="rr-banner" data-tone="warning">
                    {{ entityDetail.warning }}
                  </p>
                  <p v-if="entitySubgraph?.warning" class="rr-banner" data-tone="warning">
                    {{ entitySubgraph.warning }}
                  </p>
                </template>

                <template v-else-if="selectedGraphRelationCard">
                  <article class="detail-card">
                    <p class="rr-kicker">{{ selectedGraphRelationCard.subtitle }}</p>
                    <h4>{{ selectedGraphRelationCard.title }}</h4>
                    <p>{{ selectedGraphRelationCard.summary }}</p>
                    <div class="token-section">
                      <span class="token-section__label">{{
                        t('flow.search.graph.detail.matchReasons')
                      }}</span>
                      <div v-if="selectedGraphRelationCard.matchReasons.length" class="token-list">
                        <span
                          v-for="reason in selectedGraphRelationCard.matchReasons"
                          :key="reason"
                          class="token-chip"
                          >{{ reason }}</span
                        >
                      </div>
                      <p v-else class="rr-note">
                        {{ t('flow.search.graph.detail.noMatchReasons') }}
                      </p>
                    </div>
                  </article>
                </template>

                <EmptyStateCard
                  v-else-if="graphDetailError"
                  :title="t('flow.search.graph.detail.loadErrorTitle')"
                  :message="graphDetailError"
                  :hint="t('flow.search.graph.detail.loadErrorHint')"
                />

                <EmptyStateCard
                  v-else
                  :title="t('flow.search.graph.detail.empty.title')"
                  :message="t('flow.search.graph.detail.empty.body')"
                  :hint="t('flow.search.graph.detail.empty.hint')"
                />
              </div>

              <details
                class="graph-diagnostics technical-details"
                :open="showGraphReadinessDetails"
              >
                <summary @click.prevent="showGraphReadinessDetails = !showGraphReadinessDetails">
                  <span>{{ t('flow.search.graph.diagnostics.title') }}</span>
                  <small>{{ t('flow.search.graph.diagnostics.description') }}</small>
                </summary>

                <div class="diagnostics-block">
                  <h5>{{ t('flow.search.graph.diagnostics.blockersTitle') }}</h5>
                  <ul
                    v-if="graphDiagnosticsBlockers.length"
                    class="bullet-list bullet-list--compact"
                  >
                    <li v-for="blocker in graphDiagnosticsBlockers" :key="blocker">
                      {{ blocker }}
                    </li>
                  </ul>
                  <p v-else class="rr-note">{{ t('flow.search.graph.diagnostics.noBlockers') }}</p>
                </div>

                <div class="diagnostics-block">
                  <h5>{{ t('flow.search.graph.diagnostics.nextStepsTitle') }}</h5>
                  <ul
                    v-if="graphDiagnosticsNextSteps.length"
                    class="bullet-list bullet-list--compact"
                  >
                    <li v-for="step in graphDiagnosticsNextSteps" :key="step">{{ step }}</li>
                  </ul>
                  <p v-else class="rr-note">{{ t('flow.search.graph.diagnostics.noNextSteps') }}</p>
                </div>

                <div class="graph-context-panel__metrics graph-context-panel__metrics--diagnostics">
                  <article class="metric-card">
                    <span class="metric-card__label">{{
                      t('flow.search.graph.metrics.documents')
                    }}</span>
                    <strong>{{
                      graphContentSummary
                        ? formatCount(graphContentSummary.persisted_document_count, 'document')
                        : '—'
                    }}</strong>
                  </article>
                  <article class="metric-card">
                    <span class="metric-card__label">{{
                      t('flow.search.graph.metrics.chunks')
                    }}</span>
                    <strong>{{
                      graphContentSummary
                        ? formatCount(graphContentSummary.persisted_chunk_count, 'chunk')
                        : '—'
                    }}</strong>
                  </article>
                  <article class="metric-card">
                    <span class="metric-card__label">{{
                      t('flow.search.graph.metrics.entityRefs')
                    }}</span>
                    <strong>{{
                      graphProvenanceSummary
                        ? formatCount(graphProvenanceSummary.entities_with_chunk_refs, 'entity')
                        : '—'
                    }}</strong>
                  </article>
                  <article class="metric-card">
                    <span class="metric-card__label">{{
                      t('flow.search.graph.metrics.relationRefs')
                    }}</span>
                    <strong>{{
                      graphProvenanceSummary
                        ? formatCount(graphProvenanceSummary.relations_with_chunk_refs, 'relation')
                        : '—'
                    }}</strong>
                  </article>
                </div>
              </details>
            </div>
          </details>
        </div>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.chat-page {
  gap: var(--rr-space-6);
}

.chat-page__layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--rr-space-5);
  align-items: start;
}

.chat-page__layout--single {
  grid-template-columns: minmax(0, 1fr);
}

.chat-page__main,
.chat-page__conversation,
.session-sidebar,
.session-sidebar__body,
.ask-panel,
.answer-panel,
.empty-answer,
.timeline-panel,
.mobile-session-bar,
.graph-context-panel,
.graph-context-panel__search,
.graph-results,
.graph-detail,
.graph-diagnostics,
.diagnostics-block {
  display: grid;
  gap: var(--rr-space-5);
}

.chat-page__main,
.chat-page__conversation {
  min-width: 0;
}

.ask-panel__header,
.answer-panel__header,
.timeline-panel__header,
.session-sidebar__summary,
.mobile-session-bar,
.graph-context-panel__header,
.graph-detail__header,
.graph-context-panel__summary {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-5);
  align-items: flex-start;
}

.ask-panel__copy,
.answer-panel__copy,
.session-sidebar__summary,
.mobile-session-bar__copy {
  display: grid;
  gap: 6px;
}

.graph-context-panel__summary {
  cursor: pointer;
  list-style: none;
}

.graph-context-panel__summary::-webkit-details-marker {
  display: none;
}

.graph-context-panel__summary-meta {
  display: grid;
  justify-items: end;
  gap: 8px;
}

.ask-composer {
  position: sticky;
  bottom: calc(96px + env(safe-area-inset-bottom, 0px));
  z-index: 12;
  box-shadow: 0 -8px 28px rgb(15 23 42 / 0.08);
}

.session-item,
.timeline-item,
.mobile-session-bar,
.graph-result,
.metric-card,
.detail-card,
.diagnostics-block {
  border: 1px solid var(--rr-color-border);
  border-radius: var(--rr-radius-lg);
  background: rgba(255, 255, 255, 0.03);
}

.timeline-item,
.mobile-session-bar,
.graph-result,
.metric-card,
.detail-card,
.diagnostics-block {
  padding: var(--rr-space-3);
}

.timeline-list,
.graph-results__list,
.graph-detail__grid,
.graph-context-panel__metrics,
.token-section,
.token-list,
.bullet-list {
  display: grid;
  gap: var(--rr-space-3);
}

.session-item {
  padding: var(--rr-space-3);
  text-align: left;
  display: grid;
  gap: 6px;
  background: transparent;
}

.session-item[data-active='true'],
.graph-result[data-active='true'] {
  border-color: var(--rr-color-accent);
  background: rgba(121, 182, 255, 0.08);
}

.session-item span,
.session-item small,
.timeline-item__meta span,
.metric-card__label,
.graph-result__kind,
.token-section__label {
  color: var(--rr-color-text-muted);
}

.timeline-list {
  max-height: min(58vh, 640px);
  overflow: auto;
  padding-right: 4px;
}

.timeline-item {
  display: grid;
  gap: 8px;
}

.timeline-item[data-role='assistant'] {
  border-color: rgba(121, 182, 255, 0.35);
}

.timeline-item__meta {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  font-size: 0.85rem;
}

.answer-copy,
.timeline-item p,
.graph-result p {
  white-space: pre-wrap;
  margin: 0;
}

.ask-panel__examples {
  display: grid;
  gap: var(--rr-space-3);
}

.ask-panel__examples-label {
  margin: 0;
}

.ask-panel__examples-list,
.token-list {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
}

.ask-panel__example {
  justify-content: flex-start;
  text-align: left;
}

.ask-panel__actions {
  justify-content: space-between;
  align-items: flex-end;
}

.ask-panel__meta {
  display: grid;
  gap: 4px;
  justify-items: end;
}

.ask-panel__status {
  font-size: 0.86rem;
}

.ask-panel__submit {
  min-width: 136px;
}

.session-sidebar__summary {
  cursor: pointer;
  list-style: none;
}

.session-sidebar__summary::-webkit-details-marker {
  display: none;
}

.session-sidebar__summary h3,
.graph-context-panel h3,
.graph-detail h4,
.graph-diagnostics h4,
.graph-results h4 {
  margin: 0;
}

.session-sidebar__body {
  margin-top: var(--rr-space-3);
}

.ask-panel .rr-button--ghost {
  white-space: nowrap;
}

.answer-panel {
  display: grid;
  gap: var(--rr-space-4);
}

.answer-panel .rr-banner {
  margin: 0;
}

.answer-body__label {
  display: block;
  margin-bottom: var(--rr-space-2);
  color: var(--rr-color-text-muted);
  font-size: 0.85rem;
}

.ask-panel .rr-button,
.empty-actions .rr-button {
  text-decoration: none;
}

.ask-panel .rr-button--ghost {
  padding-inline: 0;
}

.ask-panel .rr-button--ghost:hover {
  padding-inline: 0;
}

.empty-actions {
  display: flex;
}

.graph-context-panel__metrics {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.graph-context-panel__metrics--diagnostics,
.graph-detail__grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.graph-context-panel__body {
  display: grid;
  gap: var(--rr-space-5);
  grid-template-columns: minmax(0, 1.1fr) minmax(0, 1.2fr) minmax(0, 1fr);
}

.graph-result {
  display: grid;
  gap: var(--rr-space-2);
  text-align: left;
}

.graph-result__meta {
  display: grid;
  gap: 4px;
}

.technical-details {
  padding: 0.875rem 1rem;
  border: 1px dashed var(--rr-color-border);
  border-radius: var(--rr-radius-md);
  background: rgba(255, 255, 255, 0.03);
}

.technical-details summary {
  display: grid;
  gap: 0.25rem;
  cursor: pointer;
  list-style: none;
}

.technical-details summary::-webkit-details-marker {
  display: none;
}

.token-chip {
  border-radius: 999px;
  padding: 0.35rem 0.75rem;
  background: rgba(121, 182, 255, 0.12);
  border: 1px solid rgba(121, 182, 255, 0.28);
}

.token-chip--mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
  font-size: 0.8125rem;
}

.bullet-list {
  margin: 0;
  padding-left: 1.25rem;
  list-style: disc;
}

.bullet-list--compact {
  gap: 0.35rem;
}

@media (max-width: 1200px) {
  .graph-context-panel__body,
  .graph-context-panel__metrics,
  .graph-context-panel__metrics--diagnostics,
  .graph-detail__grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 900px) {
  .chat-page {
    gap: var(--rr-space-4);
  }

  .chat-page__main,
  .chat-page__conversation {
    gap: var(--rr-space-4);
  }

  .session-sidebar {
    position: relative;
    order: 0;
  }

  .ask-composer {
    order: 2;
    margin-top: auto;
  }

  .timeline-list {
    max-height: none;
  }
}

@media (max-width: 720px) {
  .ask-panel__header,
  .answer-panel__header,
  .timeline-panel__header,
  .mobile-session-bar,
  .session-sidebar__summary,
  .graph-context-panel__header,
  .graph-detail__header {
    flex-direction: column;
  }

  .ask-panel__actions {
    align-items: stretch;
    flex-direction: column;
  }

  .ask-panel__meta {
    justify-items: flex-start;
  }

  .ask-panel__submit {
    width: 100%;
  }
}
</style>
const shouldAutoExpandGraphContext = computed(() => Boolean( graphCoverageWarning.value ||
graphSurfaceError.value || graphSearchError.value || graphDetailError.value ||
(selectedGraphCard.value && graphPanelStatus.value.status !== 'unavailable') ||
graphPanelStatus.value.status === 'degraded', ), ) watch( shouldAutoExpandGraphContext, (value) => {
if (value) { showGraphContext.value = true } }, { immediate: true }, )
