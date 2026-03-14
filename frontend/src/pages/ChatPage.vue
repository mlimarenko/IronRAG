<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  fetchProjects,
  fetchRetrievalRunDetail,
  fetchWorkspaces,
  runQuery,
  type QueryResponseSurface,
  type RetrievalRunDetail,
} from 'src/boot/api'
import RetrievalDiagnosticsPanel from 'src/components/chat/RetrievalDiagnosticsPanel.vue'
import StatusPill from 'src/components/chat/StatusPill.vue'
import TokenListSection from 'src/components/chat/TokenListSection.vue'
import PageSection from 'src/components/shell/PageSection.vue'
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

const { t } = useI18n()

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

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

  return { status: 'draft', label: t('flow.search.statusDraft') }
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = syncSelectedWorkspaceId(workspaces.value)
  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    syncSelectedProjectId(projects.value)
  } else {
    projects.value = []
    syncSelectedProjectId([])
  }
})

async function submitQuery() {
  loading.value = true
  errorMessage.value = null
  result.value = null
  detail.value = null
  try {
    if (!selectedProjectId.value) {
      throw new Error('Create and select a collection in Setup before searching.')
    }

    const response = await runQuery({
      project_id: selectedProjectId.value,
      query_text: queryText.value.trim(),
      top_k: 8,
    })

    result.value = response
    try {
      detail.value = await fetchRetrievalRunDetail(response.retrieval_run_id)
    } catch {
      detail.value = null
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown search error'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="rr-page-grid chat-page">
    <PageSection
      :eyebrow="t('flow.search.eyebrow')"
      :title="t('flow.search.title')"
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/ingest">
          {{ t('flow.search.action') }}
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.search.stats.workspace') }}</p>
          <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.search.stats.project') }}</p>
          <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.search.stats.answer') }}</p>
          <strong>{{ result?.answer_status ?? t('flow.search.stats.idle') }}</strong>
        </article>
      </div>

      <article class="rr-panel rr-panel--accent query-panel">
        <div class="query-panel__heading">
          <h3>{{ t('flow.search.query.title') }}</h3>
          <StatusPill :status="result?.answer_status ?? (selectedProject ? 'ready' : 'blocked')" />
        </div>

        <label class="rr-field">
          <span class="rr-field__label">{{ t('flow.search.query.question') }}</span>
          <textarea
            v-model="queryText"
            class="rr-control"
            rows="5"
            placeholder="Ask about indexed content"
          />
        </label>

        <div class="rr-action-row">
          <button
            type="button"
            class="rr-button"
            :disabled="loading || !selectedProjectId || !queryText.trim()"
            @click="submitQuery"
          >
            {{ loading ? t('flow.search.query.actionBusy') : t('flow.search.query.action') }}
          </button>
        </div>
      </article>

      <p v-if="errorMessage" class="rr-banner" data-tone="danger">
        {{ errorMessage }}
      </p>

      <article v-if="result" class="rr-panel result-panel">
        <div class="result-panel__header">
          <h3>{{ t('flow.search.result.title') }}</h3>
          <StatusPill :status="result.answer_status" />
        </div>

        <div class="rr-stat-strip">
          <div class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.search.result.mode') }}</p>
            <strong>{{ result.mode }}</strong>
          </div>
          <div class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.search.result.grounding') }}</p>
            <strong>{{ result.weak_grounding ? t('flow.search.result.groundingWeak') : t('flow.search.result.groundingOk') }}</strong>
          </div>
          <div class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.search.result.references') }}</p>
            <strong>{{ result.references.length }}</strong>
          </div>
        </div>

        <p class="answer-copy">{{ result.answer }}</p>

        <p v-if="result.warning" class="rr-banner" data-tone="warning">
          {{ t('flow.search.result.warningPrefix') }}: {{ result.warning }}
        </p>

        <TokenListSection
          :title="t('flow.search.result.listTitle')"
          :empty-message="t('flow.search.result.listEmpty')"
          :items="result.references"
        />
      </article>

      <article v-else class="rr-panel rr-panel--muted">
        <p class="rr-note">{{ t('flow.search.result.waitingBody') }}</p>
      </article>

      <RetrievalDiagnosticsPanel v-if="detail" :detail="detail" />
    </PageSection>
  </section>
</template>

<style scoped>
.query-panel,
.result-panel {
  gap: var(--rr-space-5);
}

.query-panel__heading,
.result-panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
}

.query-panel__heading h3,
.result-panel__header h3 {
  margin: 0;
  font-size: 1rem;
}

.answer-copy {
  margin: 0;
  white-space: pre-wrap;
  line-height: 1.65;
  color: var(--rr-color-text-primary);
}

@media (width <= 700px) {
  .query-panel__heading,
  .result-panel__header {
    flex-direction: column;
  }
}
</style>
