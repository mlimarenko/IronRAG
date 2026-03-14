<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  createSource,
  fetchDocuments,
  fetchIngestionJobDetail,
  fetchProjects,
  fetchSources,
  fetchWorkspaces,
  ingestText,
  type DocumentSummary,
  type IngestionJobDetail,
  type SourceSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
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
const documents = ref<DocumentSummary[]>([])
const sources = ref<SourceSummary[]>([])
const sourceLabel = ref('Pasted text')
const externalKey = ref(`note-${String(Date.now())}`)
const title = ref('')
const text = ref('')
const statusMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)
const latestJob = ref<IngestionJobDetail | null>(null)

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const pageStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: t('flow.library.statusBlocked') }
  }

  if (documents.value.length > 0) {
    return { status: 'ready', label: t('flow.library.documentsCount', { count: documents.value.length }) }
  }

  return { status: 'draft', label: t('flow.library.statusDraft') }
})

async function loadProjectData(projectId: string) {
  const [docs, srcs] = await Promise.all([fetchDocuments(projectId), fetchSources(projectId)])
  documents.value = docs
  sources.value = srcs
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms)
  })
}

async function waitForIngestionJob(jobId: string): Promise<IngestionJobDetail | null> {
  const terminalStatuses = new Set(['completed', 'failed', 'retryable_failed', 'canceled'])

  for (let attempt = 0; attempt < 10; attempt += 1) {
    const detail = await fetchIngestionJobDetail(jobId)
    latestJob.value = detail

    if (terminalStatuses.has(detail.status)) {
      return detail
    }

    await sleep(700)
  }

  return latestJob.value
}

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
  if (selectedProjectId.value) {
    await loadProjectData(selectedProjectId.value)
  }
})

async function ingestCurrentText() {
  errorMessage.value = null
  statusMessage.value = null
  loading.value = true
  latestJob.value = null

  if (!selectedProjectId.value) {
    errorMessage.value = 'Create and select a project first in Setup.'
    loading.value = false
    return
  }

  try {
    let sourceId = sources.value[0]?.id
    if (!sourceId) {
      const source = await createSource({
        project_id: selectedProjectId.value,
        source_kind: 'text',
        label: sourceLabel.value.trim() || 'Pasted text',
      })
      sourceId = source.id
      sources.value = [source, ...sources.value.filter((item) => item.id !== source.id)]
    }

    const result = await ingestText({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      external_key: externalKey.value.trim(),
      title: title.value.trim() || null,
      text: text.value,
    })

    statusMessage.value = `Queued ingestion job ${result.ingestion_job_id}. Waiting for completion...`

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)
    if (jobDetail?.status === 'completed') {
      statusMessage.value = `Completed ingestion job ${jobDetail.id}. Indexed content is now visible below.`
      text.value = ''
      title.value = ''
      externalKey.value = `note-${String(Date.now())}`
      return
    }

    if (jobDetail?.error_message) {
      errorMessage.value = `Ingestion job ${jobDetail.id} failed: ${jobDetail.error_message}`
      statusMessage.value = null
      return
    }

    const status = jobDetail?.status ?? result.status
    const stage = jobDetail?.stage ?? result.stage
    statusMessage.value = `Ingestion job ${result.ingestion_job_id} is ${status} at stage ${stage}.`
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Failed to ingest text'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="rr-page-grid ingestion-page">
    <PageSection
      :eyebrow="t('flow.library.eyebrow')"
      :title="t('flow.library.title')"
      :description="t('flow.library.description')"
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/ask">
          {{ t('flow.library.action') }}
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.library.stats.workspace') }}</p>
          <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.library.stats.project') }}</p>
          <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.library.stats.documents') }}</p>
          <strong>{{ documents.length }}</strong>
          <p>{{ t('flow.library.stats.documentsHint') }}</p>
        </article>
      </div>

      <p v-if="statusMessage" class="rr-banner" data-tone="success">
        {{ statusMessage }}
      </p>
      <p v-if="errorMessage" class="rr-banner" data-tone="danger">
        {{ errorMessage }}
      </p>

      <div class="ingestion-grid">
        <article class="rr-panel rr-panel--accent rr-stack">
          <div class="ingestion-panel__heading">
            <div>
              <p class="rr-kicker">{{ t('flow.library.form.kicker') }}</p>
              <h3>{{ t('flow.library.form.title') }}</h3>
            </div>
            <StatusBadge
              :status="selectedProjectId ? 'ready' : 'blocked'"
              :label="selectedProjectId ? t('flow.library.form.ready') : t('flow.library.form.needsSetup')"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.library.form.sourceLabel') }}</span>
              <input v-model="sourceLabel" class="rr-control" type="text" placeholder="Pasted text">
            </label>
            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.externalKey') }}</span>
                <input v-model="externalKey" class="rr-control" type="text" placeholder="note-001">
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.titleLabel') }}</span>
                <input
                  v-model="title"
                  class="rr-control"
                  type="text"
                  placeholder="Internal handbook excerpt"
                >
              </label>
            </div>
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.library.form.text') }}</span>
              <textarea
                v-model="text"
                class="rr-control"
                rows="12"
                placeholder="Paste the content you want RustRAG to index"
              />
            </label>
          </div>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!selectedProjectId || !text.trim() || loading"
              @click="ingestCurrentText"
            >
              {{ loading ? t('flow.library.form.actionBusy') : t('flow.library.form.action') }}
            </button>
          </div>
        </article>

        <div class="ingestion-side rr-grid">
          <article class="rr-panel">
            <div class="ingestion-panel__heading">
              <div>
                <p class="rr-kicker">{{ t('flow.library.lists.documents.kicker') }}</p>
                <h3>{{ t('flow.library.lists.documents.title') }}</h3>
              </div>
              <StatusBadge
                :status="documents.length ? 'ready' : 'draft'"
                :label="documents.length ? t('flow.library.lists.documents.ready') : t('flow.library.lists.documents.empty')"
              />
            </div>

            <p v-if="!documents.length" class="rr-note">
              {{ t('flow.library.lists.documents.emptyMessage') }}
            </p>
            <ul v-else class="rr-list">
              <li v-for="document in documents" :key="document.id">
                <strong>{{ document.title || document.external_key }}</strong>
                <span class="rr-muted">{{ document.status ?? 'Indexed' }}</span>
              </li>
            </ul>
          </article>

          <article class="rr-panel rr-panel--muted">
            <div class="ingestion-panel__heading">
              <div>
                <p class="rr-kicker">{{ t('flow.library.lists.sources.kicker') }}</p>
                <h3>{{ t('flow.library.lists.sources.title') }}</h3>
              </div>
              <StatusBadge
                :status="sources.length ? 'ready' : 'draft'"
                :label="sources.length ? t('flow.library.lists.sources.ready') : t('flow.library.lists.sources.empty')"
              />
            </div>

            <p v-if="!sources.length" class="rr-note">
              {{ t('flow.library.lists.sources.emptyMessage') }}
            </p>
            <ul v-else class="rr-list">
              <li v-for="source in sources" :key="source.id">
                <strong>{{ source.label }}</strong>
                <span class="rr-muted">{{ source.source_kind }} · {{ source.status }}</span>
              </li>
            </ul>
          </article>

          <article v-if="latestJob" class="rr-panel rr-panel--muted">
            <div class="ingestion-panel__heading">
              <div>
                <p class="rr-kicker">{{ t('flow.library.lists.job.kicker') }}</p>
                <h3>{{ t('flow.library.lists.job.title') }}</h3>
              </div>
              <StatusBadge :status="latestJob.status" :label="latestJob.stage" />
            </div>

            <p class="rr-note">
              Job {{ latestJob.id }} is {{ latestJob.status }}.
              <span v-if="latestJob.error_message"> {{ latestJob.error_message }}</span>
            </p>
          </article>
        </div>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.ingestion-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.25fr) minmax(320px, 0.75fr);
  gap: var(--rr-space-4);
}

.ingestion-panel__heading {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.ingestion-panel__heading h3 {
  margin: 4px 0 0;
}

@media (width <= 1100px) {
  .ingestion-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 700px) {
  .ingestion-panel__heading {
    flex-direction: column;
  }
}
</style>
