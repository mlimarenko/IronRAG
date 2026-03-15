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
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import { formatLocaleDateTime } from 'src/lib/formatting'
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

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const hasIndexedDocuments = computed(() => (readiness.value?.documents ?? 0) > 0)
const hasIngestionRuns = computed(() => (readiness.value?.ingestion_jobs ?? 0) > 0)
const canSubmit = computed(() =>
  Boolean(
    selectedProjectId.value &&
    queryText.value.trim() &&
    !loading.value &&
    readiness.value?.ready_for_query,
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

  if (readiness.value?.ready_for_query) {
    return { status: 'draft', label: t('flow.search.statusDraft') }
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

  if (readiness.value?.ready_for_query) {
    return t('flow.search.query.hintReady')
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

  if (!readiness.value?.ready_for_query) {
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
    if (!selectedProject.value || readiness.value?.ready_for_query) {
      return null
    }

    if (!hasIndexedDocuments.value && !hasIngestionRuns.value) {
      return {
        tone: 'info',
        title: t('flow.search.readiness.emptyState.title'),
        message: t('flow.search.readiness.emptyState.body'),
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

watch(() => selectedProjectId.value, async (projectId) => {
  const scopedProjectId = ensureProjectMatchesWorkspace(projects.value, projectId)
  result.value = null
  detail.value = null
  errorMessage.value = null
  readiness.value = null
  sessions.value = []
  messages.value = []
  activeSessionId.value = ''
  showMobileSessions.value = false

  if (!scopedProjectId) {
    return
  }

  if (scopedProjectId !== projectId) {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-call
    setSelectedProjectId(scopedProjectId)
  }

  await loadReadiness(scopedProjectId)
  await loadSessions(scopedProjectId)
})

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
    await Promise.all([loadReadiness(scope.projectId), loadSessions(scope.projectId)])
  }
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', updateViewportWidth)
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

    if (!readiness.value?.ready_for_query) {
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

    await Promise.all([loadSessions(selectedProjectId.value), loadMessages(response.session_id)])

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
        <RouterLink class="rr-button rr-button--secondary" to="/files">
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
                  <strong>{{
                    session.title ||
                    t('flow.search.sessions.fallbackTitle', { id: session.id.slice(0, 8) })
                  }}</strong>
                  <span>{{
                    session.last_message_preview || t('flow.search.sessions.emptyPreview')
                  }}</span>
                  <small>{{
                    t('flow.search.sessions.count', { count: session.message_count })
                  }}</small>
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
                <RouterLink class="rr-button rr-button--secondary" to="/processing">
                  {{ t('flow.search.empty.noProject.action') }}
                </RouterLink>
              </template>
            </EmptyStateCard>

            <EmptyStateCard
              v-else-if="!readiness?.ready_for_query"
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
                <RouterLink class="rr-button rr-button--secondary" to="/files">
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

              <RouterLink class="rr-button rr-button--ghost" to="/files">
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
                rows="isMobile ? 3 : 4"
                :placeholder="t('flow.search.query.placeholder')"
                :disabled="!selectedProject"
                @keydown="handleTextareaKeydown"
              />
            </label>

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
.mobile-session-bar {
  display: grid;
  gap: var(--rr-space-5);
}

.chat-page__main {
  min-width: 0;
}

.chat-page__conversation {
  min-width: 0;
}

.ask-panel__header,
.answer-panel__header,
.timeline-panel__header,
.session-sidebar__summary,
.mobile-session-bar {
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

.ask-composer {
  position: sticky;
  bottom: calc(96px + env(safe-area-inset-bottom, 0px));
  z-index: 12;
  box-shadow: 0 -8px 28px rgb(15 23 42 / 0.08);
}

.session-item,
.timeline-item,
.mobile-session-bar {
  border: 1px solid var(--rr-color-border);
  border-radius: var(--rr-radius-lg);
  background: rgba(255, 255, 255, 0.03);
}

.timeline-item,
.mobile-session-bar {
  padding: var(--rr-space-3);
}

.timeline-list {
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

.session-item[data-active='true'] {
  border-color: var(--rr-color-accent);
  background: rgba(121, 182, 255, 0.08);
}

.session-item span,
.session-item small,
.timeline-item__meta span {
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
.timeline-item p {
  white-space: pre-wrap;
  margin: 0;
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

.session-sidebar__summary h3 {
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

@media (max-width: 900px) {
  .chat-page {
    gap: var(--rr-space-4);
  }

  .chat-page__main {
    gap: var(--rr-space-4);
  }

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
  .session-sidebar__summary {
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
