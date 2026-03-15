<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import {
  fetchProjects,
  fetchProjectReadiness,
  fetchRetrievalRunDetail,
  fetchWorkspaces,
  isUnauthorizedApiError,
  runQuery,
  type ProjectReadinessSummary,
  type QueryResponseSurface,
  type RetrievalRunDetail,
} from 'src/boot/api'
import ReferenceList from 'src/components/chat/ReferenceList.vue'
import RetrievalDiagnosticsPanel from 'src/components/chat/RetrievalDiagnosticsPanel.vue'
import StatusPill from 'src/components/chat/StatusPill.vue'
import CrossSurfaceGuide from 'src/components/shell/CrossSurfaceGuide.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import ProductSpine from 'src/components/shell/ProductSpine.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
} from 'src/stores/flow'

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

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const normalizedIndexingState = computed(() => readiness.value?.indexing_state.trim().toLowerCase() ?? '')
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
const answerCapabilities = computed(() => {
  if (!selectedProject.value) {
    return []
  }

  if (readiness.value?.ready_for_query) {
    return [
      t('flow.search.capabilities.ready.answer'),
      t('flow.search.capabilities.ready.verify'),
      t('flow.search.capabilities.ready.followUp'),
    ]
  }

  if (hasIndexedDocuments.value || hasIngestionRuns.value) {
    return [
      t('flow.search.capabilities.partial.answer'),
      t('flow.search.capabilities.partial.verify'),
      t('flow.search.capabilities.partial.next'),
    ]
  }

  return [
    t('flow.search.capabilities.empty.answer'),
    t('flow.search.capabilities.empty.verify'),
    t('flow.search.capabilities.empty.next'),
  ]
})
const weakContextActions = computed(() => {
  const actions = [
    {
      label: t('flow.search.nextActions.openFiles'),
      to: '/ingest',
    },
  ]

  if (selectedProject.value && (hasIndexedDocuments.value || hasIngestionRuns.value)) {
    actions.push({
      label: t('flow.search.nextActions.retryQuestion'),
      to: `/chat?q=${encodeURIComponent(queryText.value.trim() || t('flow.search.query.examples.summary'))}`,
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

watch(selectedProjectId, async (projectId) => {
  result.value = null
  detail.value = null
  errorMessage.value = null
  readiness.value = null

  if (!projectId) {
    return
  }

  await loadReadiness(projectId)
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = syncSelectedWorkspaceId(workspaces.value)
  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    const projectId = syncSelectedProjectId(projects.value)
    if (projectId) {
      await loadReadiness(projectId)
    }
  } else {
    projects.value = []
    syncSelectedProjectId([])
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
      return state?.trim() || t('flow.search.readiness.states.unknown')
  }
}

async function loadReadiness(projectId: string) {
  try {
    readiness.value = await fetchProjectReadiness(projectId)
  } catch {
    readiness.value = null
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
      query_text: trimmedQuery,
      top_k: 8,
    })

    result.value = response
    queryText.value = trimmedQuery

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
        <RouterLink class="rr-button rr-button--secondary" to="/ingest">
          {{ t('flow.search.action') }}
        </RouterLink>
      </template>

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
            <span class="answer-meta__label">{{ t('flow.search.result.grounding') }}</span>
            <strong>
              {{
                result.weak_grounding
                  ? t('flow.search.result.groundingWeak')
                  : t('flow.search.result.groundingStrong')
              }}
            </strong>
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
    </PageSection>
  </section>
</template>

<style scoped>
.chat-page {
  gap: var(--rr-space-6);
}

.ask-panel,
.answer-panel,
.empty-answer {
  gap: var(--rr-space-5);
}

.ask-panel__header,
.answer-panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-5);
  align-items: flex-start;
}

.ask-panel__copy,
.answer-panel__copy {
  display: grid;
  gap: 6px;
}

.ask-panel__copy h2,
.answer-panel__copy h3,
.empty-answer h3 {
  margin: 0;
  font-size: clamp(1.15rem, 2vw, 1.4rem);
}

.context-grid,
.answer-meta {
  display: grid;
  gap: var(--rr-space-3);
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.context-card,
.answer-meta__card,
.capability-panel {
  display: grid;
  gap: 6px;
  min-width: 0;
  padding: var(--rr-space-3) var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.74);
}

.context-card__label,
.answer-meta__label,
.answer-body__label,
.capability-panel__label {
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.context-card strong,
.answer-meta__card strong {
  font-size: 0.95rem;
  overflow-wrap: anywhere;
}

.capability-list {
  display: grid;
  gap: var(--rr-space-2);
  margin: 0;
  padding-left: 1.1rem;
  color: var(--rr-color-text-secondary);
}

.capability-list li {
  line-height: 1.5;
}

.answer-details-toggle {
  border-radius: var(--rr-radius-lg);
}

.answer-details-toggle summary {
  cursor: pointer;
  list-style: none;
  padding: 0.95rem 1.1rem;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.7);
  font-weight: 700;
  color: var(--rr-color-text-secondary);
}

.answer-details-toggle[open] summary {
  margin-bottom: var(--rr-space-3);
}

.ask-panel__input {
  min-height: 140px;
}

.query-examples {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-2);
  align-items: center;
}

.query-examples__label {
  font-size: 0.84rem;
  font-weight: 700;
  color: var(--rr-color-text-secondary);
}

.query-example {
  display: inline-flex;
  align-items: center;
  min-height: 34px;
  padding: 0 var(--rr-space-3);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-pill);
  background: rgb(255 255 255 / 0.78);
  color: var(--rr-color-text-primary);
  cursor: pointer;
  transition:
    border-color var(--rr-motion-base),
    transform var(--rr-motion-base),
    background var(--rr-motion-base);
}

.query-example:hover,
.query-example:focus-visible {
  border-color: var(--rr-color-border-focus);
  background: rgb(255 255 255 / 0.94);
  transform: translateY(-1px);
}

.query-example:focus-visible {
  outline: none;
}

.ask-panel__actions {
  justify-content: space-between;
}

.answer-panel__summary {
  margin: 0;
  color: var(--rr-color-text-secondary);
}

.answer-body {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-md);
  background: linear-gradient(180deg, rgb(29 78 216 / 0.05), rgb(255 255 255 / 0));
}

.answer-copy {
  margin: 0;
  white-space: pre-wrap;
  line-height: 1.7;
  color: var(--rr-color-text-primary);
}

.empty-answer {
  justify-items: start;
}

.empty-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
}

@media (width <= 900px) {
  .ask-panel__header,
  .answer-panel__header,
  .ask-panel__actions {
    flex-direction: column;
    align-items: flex-start;
  }

  .context-grid,
  .answer-meta {
    width: 100%;
  }
}

@media (width <= 700px) {
  .context-grid,
  .answer-meta {
    grid-template-columns: 1fr;
  }
}
</style>
