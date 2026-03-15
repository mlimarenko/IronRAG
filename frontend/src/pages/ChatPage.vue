<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import {
  fetchChatMessages,
  fetchChatSessions,
  fetchProjectReadiness,
  fetchProjects,
  fetchRetrievalRunDetail,
  fetchWorkspaces,
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
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
} from 'src/stores/flow'
import { ensureProjectMatchesWorkspace, syncWorkspaceProjectScope } from 'src/lib/flowSelection'

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

const { t } = useI18n()
const route = useRoute()

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const queryInputRef = ref<HTMLTextAreaElement | null>(null)

const queryText = ref('')
const result = ref<QueryResponseSurface | null>(null)
const detail = ref<RetrievalRunDetail | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)
const sessionLoading = ref(false)
const sessions = ref<ChatSessionSurface[]>([])
const messages = ref<ChatMessageSurface[]>([])
const activeSessionId = ref('')

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const hasIndexedDocuments = computed(() => (readiness.value?.documents ?? 0) > 0)
const hasIngestionRuns = computed(() => (readiness.value?.ingestion_jobs ?? 0) > 0)
const canSubmit = computed(
  () =>
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
const pageStatus = computed(() => {
  if (result.value) {
    return {
      status: result.value.answer_status,
      label: result.value.weak_grounding ? t('flow.search.statusWeak') : t('flow.search.statusReady'),
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
const contextItems = computed(() => [
  {
    label: t('flow.search.context.workspace'),
    value: selectedWorkspace.value?.name ?? t('flow.common.empty'),
  },
  {
    label: t('flow.search.context.project'),
    value: selectedProject.value?.name ?? t('flow.common.empty'),
  },
  {
    label: t('flow.search.context.indexing'),
    value: formatIndexingStateLabel(readiness.value?.indexing_state),
  },
  {
    label: t('flow.search.context.documents'),
    value: readiness.value ? String(readiness.value.documents) : t('flow.common.empty'),
  },
])
const queryExamples = computed(() => [
  t('flow.search.query.examples.summary'),
  t('flow.search.query.examples.risks'),
  t('flow.search.query.examples.next'),
])
const queryHint = computed(() => {
  if (!selectedProject.value) {
    return t('flow.search.query.hintBlocked')
  }

  if (activeSession.value) {
    return t('flow.search.query.hintResume', { id: activeSession.value.id.slice(0, 8) })
  }

  if (readiness.value?.ready_for_query) {
    return t('flow.search.query.hintReady')
  }

  if (hasIndexedDocuments.value || hasIngestionRuns.value) {
    return t('flow.search.query.hintIndexing')
  }

  return t('flow.search.query.hintNoContent')
})
const resultSummary = computed(() => {
  if (!result.value) {
    return ''
  }

  if (result.value.references.length > 0) {
    return t('flow.search.result.summarySupported', { count: result.value.references.length })
  }

  return t('flow.search.result.summaryNoReferences')
})
const resultModeLabel = computed(() => {
  if (!result.value) {
    return ''
  }

  return formatModeLabel(result.value.mode)
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
const readinessNotice = computed<{ tone: BannerTone; title: string; message: string } | null>(() => {
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
})
const weakContextActions = computed(() => {
  const actions = [
    {
      label: t('flow.search.nextActions.openFiles'),
      to: '/files',
    },
  ]

  if (selectedProject.value && (hasIndexedDocuments.value || hasIngestionRuns.value)) {
    actions.push({
      label: t('flow.search.nextActions.retryQuestion'),
      to: `/search?q=${encodeURIComponent(queryText.value.trim() || t('flow.search.query.examples.summary'))}`,
    })
  }

  return actions
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
    if (!sessions.value.some((session) => session.id === nextSessionId) || activeSessionId.value === nextSessionId) {
      return
    }

    await reopenSession(nextSessionId)
  },
  { immediate: true },
)

watch(selectedProjectId, async (projectId) => {
  const scopedProjectId = ensureProjectMatchesWorkspace(projects.value, projectId)
  result.value = null
  detail.value = null
  errorMessage.value = null
  readiness.value = null
  sessions.value = []
  messages.value = []
  activeSessionId.value = ''

  if (!scopedProjectId) {
    return
  }

  if (scopedProjectId !== projectId) {
    setSelectedProjectId(scopedProjectId)
  }

  await Promise.all([loadReadiness(scopedProjectId), loadSessions(scopedProjectId)])
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = getSelectedWorkspaceId() || workspaces.value[0]?.id || ''

  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    const scope = syncWorkspaceProjectScope(workspaces.value, projects.value)
    if (scope.projectId) {
      await Promise.all([loadReadiness(scope.projectId), loadSessions(scope.projectId)])
    }
  } else {
    projects.value = []
    setSelectedProjectId('')
  }
})

function formatModeLabel(mode: string) {
  const normalized = mode.trim().toLowerCase()

  if (normalized === 'gateway_live') {
    return t('flow.search.result.modeLive')
  }

  if (normalized === 'fallback') {
    return t('flow.search.result.modeFallback')
  }

  return mode
    .split(/[_-]/g)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
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
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }
  return date.toLocaleString()
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
      typeof route.query.session === 'string' && route.query.session.trim() ? route.query.session.trim() : ''
    const requestedSessionExists = requestedSessionId
      ? sessions.value.some((session) => session.id === requestedSessionId)
      : false

    if (requestedSessionExists) {
      activeSessionId.value = requestedSessionId
      await loadMessages(requestedSessionId)
      return
    }

    if (activeSessionId.value && sessions.value.some((session) => session.id === activeSessionId.value)) {
      await loadMessages(activeSessionId.value)
      return
    }

    const nextSessionId = sessions.value[0]?.id ?? ''
    activeSessionId.value = nextSessionId
    if (nextSessionId) {
      await loadMessages(nextSessionId)
    } else {
      messages.value = []
    }
  } catch {
    sessions.value = []
    messages.value = []
    activeSessionId.value = ''
  } finally {
    sessionLoading.value = false
  }
}

async function loadMessages(sessionId: string) {
  activeSessionId.value = sessionId
  try {
    messages.value = await fetchChatMessages(sessionId)
  } catch {
    messages.value = []
  }
}

function applyExampleQuery(example: string) {
  queryText.value = example
  queryInputRef.value?.focus()
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
  await loadMessages(sessionId)
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

    await Promise.all([loadSessions(selectedProjectId.value), loadMessages(response.session_id)])

    try {
      detail.value = await fetchRetrievalRunDetail(response.retrieval_run_id)
    } catch {
      detail.value = null
    }
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
  <section class="rr-page-grid chat-page">
    <PageSection
      :title="t('flow.search.title')"
      :description="t('flow.search.description')"
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/files">
          {{ t('flow.search.action') }}
        </RouterLink>
      </template>

      <div class="chat-page__layout">
        <aside class="rr-panel rr-panel--muted session-sidebar">
          <div class="session-sidebar__header">
            <p class="rr-kicker">{{ t('flow.search.sessions.kicker') }}</p>
            <h3>{{ selectedProject ? selectedProject.name : t('flow.search.sessions.titleFallback') }}</h3>
            <p class="rr-note">{{ t('flow.search.sessions.description') }}</p>
          </div>

          <div v-if="sessionLoading" class="rr-note">{{ t('flow.search.sessions.loading') }}</div>
          <div v-else-if="!sessions.length" class="rr-note">{{ t('flow.search.sessions.empty') }}</div>
          <button
            v-for="session in sessions"
            :key="session.id"
            type="button"
            class="session-item"
            :data-active="session.id === activeSessionId"
            @click="reopenSession(session.id)"
          >
            <strong>{{ session.title || t('flow.search.sessions.fallbackTitle', { id: session.id.slice(0, 8) }) }}</strong>
            <span>{{ session.last_message_preview || t('flow.search.sessions.emptyPreview') }}</span>
            <small>{{ t('flow.search.sessions.count', { count: session.message_count }) }}</small>
          </button>
        </aside>

        <div class="chat-page__main">
          <article class="rr-panel rr-panel--accent ask-panel">
            <div class="ask-panel__header">
              <div class="ask-panel__copy">
                <p class="rr-kicker">{{ t('flow.search.query.kicker') }}</p>
                <h2>{{ t('flow.search.query.title') }}</h2>
                <p class="rr-note">{{ queryHint }}</p>
              </div>

              <div class="context-grid">
                <article
                  v-for="item in contextItems"
                  :key="item.label"
                  class="context-card"
                >
                  <span class="context-card__label">{{ item.label }}</span>
                  <strong>{{ item.value }}</strong>
                </article>
              </div>
            </div>

            <p
              v-if="readinessNotice"
              class="rr-banner"
              :data-tone="readinessNotice.tone"
            >
              <strong>{{ readinessNotice.title }}</strong>
              <span>{{ readinessNotice.message }}</span>
            </p>

            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.search.query.question') }}</span>
              <textarea
                ref="queryInputRef"
                v-model="queryText"
                class="rr-control ask-panel__input"
                rows="4"
                :placeholder="t('flow.search.query.placeholder')"
                :disabled="!selectedProject"
                @keydown="handleTextareaKeydown"
              />
            </label>

            <div class="query-examples">
              <span class="query-examples__label">{{ t('flow.search.query.examplesLabel') }}</span>
              <button
                v-for="example in queryExamples"
                :key="example"
                type="button"
                class="query-example"
                @click="applyExampleQuery(example)"
              >
                {{ example }}
              </button>
            </div>

            <div class="rr-action-row ask-panel__actions">
              <button
                type="button"
                class="rr-button"
                :disabled="!canSubmit"
                @click="submitQuery"
              >
                {{ loading ? t('flow.search.query.actionBusy') : t('flow.search.query.action') }}
              </button>
              <p class="rr-note">{{ t('flow.search.query.shortcut') }}</p>
            </div>
          </article>

          <p
            v-if="errorMessage"
            class="rr-banner"
            data-tone="danger"
          >
            {{ errorMessage }}
          </p>

          <article
            v-if="timeline.length"
            class="rr-panel rr-panel--muted timeline-panel"
          >
            <div class="timeline-panel__header">
              <div>
                <p class="rr-kicker">{{ t('flow.search.timeline.kicker') }}</p>
                <h3>{{ activeSession ? activeSession.title || t('flow.search.sessions.fallbackTitle', { id: activeSession.id.slice(0, 8) }) : t('flow.search.timeline.current') }}</h3>
              </div>
              <span class="rr-note">{{ t('flow.search.timeline.count', { count: timeline.length }) }}</span>
            </div>

            <div class="timeline-list">
              <article
                v-for="message in timeline"
                :key="message.id"
                class="timeline-item"
                :data-role="message.role"
              >
                <div class="timeline-item__meta">
                  <strong>{{ message.role === 'assistant' ? t('flow.search.timeline.assistant') : t('flow.search.timeline.you') }}</strong>
                  <span>{{ formatMessageTimestamp(message.created_at) }}</span>
                </div>
                <p>{{ message.content }}</p>
              </article>
            </div>
          </article>

          <article
            v-if="result"
            class="rr-panel answer-panel"
          >
            <div class="answer-panel__header">
              <div class="answer-panel__copy">
                <p class="rr-kicker">{{ t('flow.search.result.kicker') }}</p>
                <h3>{{ t('flow.search.result.title') }}</h3>
                <p class="answer-panel__summary">{{ resultSummary }}</p>
              </div>
              <StatusPill :status="result.answer_status" />
            </div>

            <div class="answer-meta">
              <article class="answer-meta__card">
                <span class="answer-meta__label">{{ t('flow.search.result.mode') }}</span>
                <strong>{{ resultModeLabel }}</strong>
              </article>
              <article class="answer-meta__card">
                <span class="answer-meta__label">{{ t('flow.search.result.references') }}</span>
                <strong>{{ result.references.length }}</strong>
              </article>
              <article class="answer-meta__card">
                <span class="answer-meta__label">{{ t('flow.search.result.session') }}</span>
                <strong>{{ result.session_id.slice(0, 8) }}</strong>
              </article>
            </div>

            <p
              v-if="resultNotice"
              class="rr-banner"
              :data-tone="resultNotice.tone"
            >
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
              <div class="empty-actions">
                <RouterLink
                  v-for="action in weakContextActions"
                  :key="action.label"
                  class="rr-button rr-button--secondary"
                  :to="action.to"
                >
                  {{ action.label }}
                </RouterLink>
              </div>
            </template>
          </EmptyStateCard>

          <article
            v-else
            class="rr-panel rr-panel--muted empty-answer"
          >
            <p class="rr-kicker">{{ t('flow.search.result.waitingKicker') }}</p>
            <h3>{{ t('flow.search.result.waitingTitle') }}</h3>
            <p class="rr-note">{{ t('flow.search.result.waitingBody') }}</p>
          </article>

          <details v-if="detail" class="answer-details-toggle">
            <summary>{{ t('flow.search.diagnostics.action') }}</summary>
            <RetrievalDiagnosticsPanel :detail="detail" />
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
  grid-template-columns: minmax(240px, 320px) minmax(0, 1fr);
  gap: var(--rr-space-5);
}

.chat-page__main,
.session-sidebar,
.ask-panel,
.answer-panel,
.empty-answer,
.timeline-panel {
  display: grid;
  gap: var(--rr-space-5);
}

.ask-panel__header,
.answer-panel__header,
.timeline-panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-5);
  align-items: flex-start;
}

.ask-panel__copy,
.answer-panel__copy,
.session-sidebar__header {
  display: grid;
  gap: 6px;
}

.context-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(120px, 1fr));
  gap: var(--rr-space-3);
}

.context-card,
.answer-meta__card,
.session-item,
.timeline-item {
  border: 1px solid var(--rr-color-border);
  border-radius: var(--rr-radius-lg);
  background: rgba(255, 255, 255, 0.03);
}

.context-card,
.answer-meta__card,
.timeline-item {
  padding: var(--rr-space-3);
}

.context-card__label,
.answer-meta__label {
  display: block;
  font-size: 0.8rem;
  color: var(--rr-color-text-muted);
  margin-bottom: 4px;
}

.query-examples,
.answer-meta,
.empty-actions,
.timeline-list {
  display: grid;
  gap: var(--rr-space-3);
}

.answer-meta {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.query-example,
.session-item {
  padding: var(--rr-space-3);
  text-align: left;
}

.session-item {
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
  max-height: 520px;
  overflow: auto;
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

@media (max-width: 1100px) {
  .chat-page__layout {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 720px) {
  .ask-panel__header,
  .answer-panel__header,
  .timeline-panel__header {
    flex-direction: column;
  }

  .context-grid,
  .answer-meta {
    grid-template-columns: 1fr;
  }
}
</style>
