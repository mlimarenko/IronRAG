<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
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

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

const queryText = ref('')
const result = ref<QueryResponseSurface | null>(null)
const detail = ref<RetrievalRunDetail | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)

const selectedWorkspaceId = ref(getSelectedWorkspaceId())
const selectedProjectId = ref(getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === selectedProjectId.value) ?? null,
)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === selectedWorkspaceId.value) ?? null,
)
const pageStatus = computed(() => {
  if (result.value) {
    return {
      status: result.value.answer_status,
      label: result.value.weak_grounding ? 'Answer returned with weak grounding' : 'Answer returned',
    }
  }

  if (!selectedProject.value) {
    return { status: 'blocked', label: 'Select a project before querying' }
  }

  return { status: 'draft', label: 'Ready to run a grounded query' }
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = syncSelectedWorkspaceId(workspaces.value)
  selectedWorkspaceId.value = workspaceId
  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    selectedProjectId.value = syncSelectedProjectId(projects.value)
  } else {
    projects.value = []
    selectedProjectId.value = syncSelectedProjectId([])
  }
})

async function submitQuery() {
  loading.value = true
  errorMessage.value = null
  result.value = null
  detail.value = null
  try {
    if (!selectedProjectId.value) {
      throw new Error('Create and select a project in Setup before asking questions.')
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
    errorMessage.value = error instanceof Error ? error.message : 'Unknown query error'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="rr-page-grid chat-page">
    <PageSection
      eyebrow="Step 3"
      title="Ask grounded questions"
      description="Run a query against the active project, inspect answer status, and keep the retrieval diagnostics close to the response instead of hidden behind a different admin flow."
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/ingest">
          Back to ingest
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">Workspace</p>
          <strong>{{ selectedWorkspace?.name ?? 'Not selected' }}</strong>
          <p>Project context stays visible while evaluating answers.</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Project</p>
          <strong>{{ selectedProject?.name ?? 'Not selected' }}</strong>
          <p>{{ selectedProject ? 'Queries target this single active project.' : 'Setup must define the active project first.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Last answer</p>
          <strong>{{ result?.answer_status ?? 'No query run yet' }}</strong>
          <p>{{ result ? `${result.references.length} references returned.` : 'Run a query to inspect response quality.' }}</p>
        </article>
      </div>

      <article class="rr-panel rr-panel--accent query-panel">
        <div class="query-panel__heading">
          <div>
            <p class="rr-kicker">Query runner</p>
            <h3>Ask about the indexed content</h3>
          </div>
          <StatusPill :status="result?.answer_status ?? (selectedProject ? 'ready' : 'blocked')" />
        </div>

        <label class="rr-field">
          <span class="rr-field__label">Question</span>
          <textarea
            v-model="queryText"
            class="rr-control"
            rows="5"
            placeholder="Ask a grounded question about the indexed content"
          />
          <p class="rr-field__hint">
            Keep the prompt scoped to information you actually indexed in the selected project.
          </p>
        </label>

        <div class="rr-action-row">
          <button
            type="button"
            class="rr-button"
            :disabled="loading || !selectedProjectId || !queryText.trim()"
            @click="submitQuery"
          >
            {{ loading ? 'Running…' : 'Run query' }}
          </button>
        </div>
      </article>

      <p
        v-if="errorMessage"
        class="rr-banner"
        data-tone="danger"
      >
        {{ errorMessage }}
      </p>

      <article v-if="result" class="rr-panel result-panel">
        <div class="result-panel__header">
          <div>
            <p class="rr-kicker">Answer</p>
            <h3>Final response surface</h3>
            <p class="rr-note">This is the direct response from the query endpoint, paired with the retrieval trace below.</p>
          </div>
          <StatusPill :status="result.answer_status" />
        </div>

        <div class="rr-stat-strip">
          <div class="rr-stat">
            <p class="rr-stat__label">Mode</p>
            <strong>{{ result.mode }}</strong>
          </div>
          <div class="rr-stat">
            <p class="rr-stat__label">Grounding</p>
            <strong>{{ result.weak_grounding ? 'Weak' : 'OK' }}</strong>
          </div>
          <div class="rr-stat">
            <p class="rr-stat__label">References</p>
            <strong>{{ result.references.length }}</strong>
          </div>
        </div>

        <p class="answer-copy">{{ result.answer }}</p>

        <p
          v-if="result.warning"
          class="rr-banner"
          data-tone="warning"
        >
          Warning: {{ result.warning }}
        </p>

        <TokenListSection
          title="Evidence references"
          empty-message="No references were returned for this answer."
          :items="result.references"
        />
      </article>

      <article v-else class="rr-panel rr-panel--muted">
        <p class="rr-kicker">Waiting for first query</p>
        <h3>Run a question once Setup and Ingest are complete</h3>
        <p class="rr-note">
          The Ask page keeps the form, answer surface, and retrieval diagnostics in one place so the minimal flow stays coherent.
        </p>
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
  align-items: flex-start;
}

.query-panel__heading h3,
.result-panel__header h3 {
  margin: 4px 0 0;
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
