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
  uploadAndIngest,
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
const uploadSourceLabel = ref('Uploaded file')
const externalKey = ref(`note-${String(Date.now())}`)
const title = ref('')
const uploadTitle = ref('')
const text = ref('')
const uploadFile = ref<File | null>(null)
const uploadInputKey = ref(0)
const statusMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)
const latestJob = ref<IngestionJobDetail | null>(null)
const acceptedUploadTypes =
  '.txt,.md,.markdown,.csv,.json,.yaml,.yml,.xml,.html,.htm,.log,.rst,.toml,.ini,.cfg,.conf,.ts,.tsx,.js,.jsx,.mjs,.cjs,.py,.rs,.java,.kt,.go,.sh,.sql,.css,.scss,text/plain,text/markdown,text/csv,application/json,application/xml,text/xml'
const textLikeExtensions = new Set([
  'txt',
  'md',
  'markdown',
  'csv',
  'json',
  'yaml',
  'yml',
  'xml',
  'html',
  'htm',
  'log',
  'rst',
  'toml',
  'ini',
  'cfg',
  'conf',
  'ts',
  'tsx',
  'js',
  'jsx',
  'mjs',
  'cjs',
  'py',
  'rs',
  'java',
  'kt',
  'go',
  'sh',
  'sql',
  'css',
  'scss',
])

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

function getFileExtension(fileName: string): string {
  const segments = fileName.toLowerCase().split('.')
  return segments.length > 1 ? segments.at(-1) ?? '' : ''
}

function isBlockedBinaryUpload(file: File): boolean {
  const extension = getFileExtension(file.name)
  return extension === 'pdf' || file.type === 'application/pdf' || file.type.startsWith('image/')
}

function isTextLikeUpload(file: File): boolean {
  const extension = getFileExtension(file.name)
  return (
    file.type.startsWith('text/') ||
    file.type === 'application/json' ||
    file.type === 'application/xml' ||
    file.type === 'text/xml' ||
    textLikeExtensions.has(extension)
  )
}

function upsertSource(source: SourceSummary) {
  sources.value = [source, ...sources.value.filter((item) => item.id !== source.id)]
}

async function ensureSource(sourceKind: string, label: string): Promise<string> {
  const existing = sources.value.find((item) => item.source_kind === sourceKind)
  if (existing) {
    return existing.id
  }

  const source = await createSource({
    project_id: selectedProjectId.value!,
    source_kind: sourceKind,
    label: label.trim() || (sourceKind === 'upload' ? 'Uploaded file' : 'Pasted text'),
  })
  upsertSource(source)
  return source.id
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

function handleUploadFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  uploadFile.value = input.files?.[0] ?? null
  if (uploadFile.value && !uploadTitle.value.trim()) {
    uploadTitle.value = uploadFile.value.name
  }
  if (uploadFile.value && isBlockedBinaryUpload(uploadFile.value)) {
    errorMessage.value = t('flow.library.upload.blockedError')
    statusMessage.value = null
    return
  }
  if (uploadFile.value && !isTextLikeUpload(uploadFile.value)) {
    errorMessage.value = t('flow.library.upload.unsupportedError')
    statusMessage.value = null
    return
  }
  errorMessage.value = null
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
    errorMessage.value = 'Create and select a collection first in Setup.'
    loading.value = false
    return
  }

  try {
    const sourceId = await ensureSource('text', sourceLabel.value)

    const result = await ingestText({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      external_key: externalKey.value.trim(),
      title: title.value.trim() || null,
      text: text.value,
    })

    statusMessage.value = `Started processing run ${result.ingestion_job_id}. Waiting for completion...`

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)
    if (jobDetail?.status === 'completed') {
      statusMessage.value = `Completed processing run ${jobDetail.id}. Indexed content is now visible below.`
      text.value = ''
      title.value = ''
      externalKey.value = `note-${String(Date.now())}`
      return
    }

    if (jobDetail?.error_message) {
      errorMessage.value = `Processing run ${jobDetail.id} failed: ${jobDetail.error_message}`
      statusMessage.value = null
      return
    }

    const status = jobDetail?.status ?? result.status
    const stage = jobDetail?.stage ?? result.stage
    statusMessage.value = `Processing run ${result.ingestion_job_id} is ${status} at stage ${stage}.`
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Failed to index text'
  } finally {
    loading.value = false
  }
}

async function uploadCurrentFile() {
  errorMessage.value = null
  statusMessage.value = null
  loading.value = true
  latestJob.value = null

  if (!selectedProjectId.value) {
    errorMessage.value = 'Create and select a collection first in Setup.'
    loading.value = false
    return
  }

  if (!uploadFile.value) {
    errorMessage.value = 'Choose a file before uploading.'
    loading.value = false
    return
  }

  if (isBlockedBinaryUpload(uploadFile.value)) {
    errorMessage.value = t('flow.library.upload.blockedError')
    loading.value = false
    return
  }

  if (!isTextLikeUpload(uploadFile.value)) {
    errorMessage.value = t('flow.library.upload.unsupportedError')
    loading.value = false
    return
  }

  try {
    const sourceId = await ensureSource('upload', uploadSourceLabel.value)
    const result = await uploadAndIngest({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      title: uploadTitle.value.trim() || null,
      file: uploadFile.value,
    })

    statusMessage.value = `Started processing run ${result.ingestion_job_id}. Waiting for completion...`

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)
    if (jobDetail?.status === 'completed') {
      statusMessage.value = `Completed processing run ${jobDetail.id}. Indexed content is now visible below.`
      uploadTitle.value = ''
      uploadFile.value = null
      uploadInputKey.value += 1
      return
    }

    if (jobDetail?.error_message) {
      errorMessage.value = `Processing run ${jobDetail.id} failed: ${jobDetail.error_message}`
      statusMessage.value = null
      return
    }

    const status = jobDetail?.status ?? result.status
    const stage = jobDetail?.stage ?? result.stage
    statusMessage.value = `Processing run ${result.ingestion_job_id} is ${status} at stage ${stage}.`
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Failed to upload and index file'
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
        </article>
      </div>

      <p v-if="statusMessage" class="rr-banner" data-tone="success">
        {{ statusMessage }}
      </p>
      <p v-if="errorMessage" class="rr-banner" data-tone="danger">
        {{ errorMessage }}
      </p>

      <div class="ingestion-grid">
        <div class="ingestion-primary rr-grid">
          <article class="rr-panel rr-panel--accent rr-stack">
            <div class="ingestion-panel__heading">
              <h3>{{ t('flow.library.form.title') }}</h3>
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
                    placeholder="Support policy excerpt"
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

          <article class="rr-panel rr-stack">
            <div class="ingestion-panel__heading">
              <h3>{{ t('flow.library.upload.title') }}</h3>
              <StatusBadge
                :status="selectedProjectId ? 'ready' : 'blocked'"
                :label="selectedProjectId ? t('flow.library.upload.ready') : t('flow.library.upload.needsSetup')"
              />
            </div>

            <p class="rr-banner" data-tone="info">
              {{ t('flow.library.upload.supportedHint') }} {{ t('flow.library.upload.blockedHint') }}
            </p>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.upload.sourceLabel') }}</span>
                <input v-model="uploadSourceLabel" class="rr-control" type="text" placeholder="Uploaded file">
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.upload.titleLabel') }}</span>
                <input
                  v-model="uploadTitle"
                  class="rr-control"
                  type="text"
                  placeholder="Knowledge-base article"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.upload.file') }}</span>
                <input
                  :key="uploadInputKey"
                  class="rr-control"
                  type="file"
                  :accept="acceptedUploadTypes"
                  @change="handleUploadFileChange"
                >
              </label>
            </div>

            <p v-if="uploadFile" class="rr-note">
              {{ t('flow.library.upload.selected') }}: {{ uploadFile.name }}
            </p>

            <div class="rr-action-row">
              <button
                type="button"
                class="rr-button"
                :disabled="!selectedProjectId || !uploadFile || loading"
                @click="uploadCurrentFile"
              >
                {{ loading ? t('flow.library.upload.actionBusy') : t('flow.library.upload.action') }}
              </button>
            </div>
          </article>
        </div>

        <div class="ingestion-side rr-grid">
          <article class="rr-panel">
            <div class="ingestion-panel__heading">
              <h3>{{ t('flow.library.lists.documents.title') }}</h3>
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
              <h3>{{ t('flow.library.lists.sources.title') }}</h3>
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
              <h3>{{ t('flow.library.lists.job.title') }}</h3>
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

.ingestion-primary {
  gap: var(--rr-space-4);
}

.ingestion-panel__heading {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
}

.ingestion-panel__heading h3 {
  margin: 0;
  font-size: 1rem;
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
