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

type UploadSupportStatus = 'supported_now' | 'planned' | 'unsupported'
type UploadFileKind = 'text_like' | 'pdf' | 'image' | 'binary'

interface UploadSelectionState {
  supportStatus: UploadSupportStatus
  fileKind: UploadFileKind
  fileKindLabel: string
  badgeLabel: string
  badgeTone: 'positive' | 'warning' | 'negative'
  bannerTone: 'success' | 'warning' | 'danger'
  message: string
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
const uploadInputRef = ref<HTMLInputElement | null>(null)
const uploadInputKey = ref(0)
const isUploadDragActive = ref(false)
const statusMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)
const latestJob = ref<IngestionJobDetail | null>(null)
const acceptedUploadTypes =
  '.txt,.md,.markdown,.csv,.json,.yaml,.yml,.xml,.html,.htm,.log,.rst,.toml,.ini,.cfg,.conf,.ts,.tsx,.js,.jsx,.mjs,.cjs,.py,.rs,.java,.kt,.go,.sh,.sql,.css,.scss,.pdf,.png,.jpg,.jpeg,.gif,.bmp,.webp,.svg,.tif,.tiff,.heic,.heif,text/plain,text/markdown,text/csv,application/json,application/xml,text/xml,application/pdf,image/*'
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
const imageExtensions = new Set([
  'png',
  'jpg',
  'jpeg',
  'gif',
  'bmp',
  'webp',
  'svg',
  'tif',
  'tiff',
  'heic',
  'heif',
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
    return {
      status: 'ready',
      label: t('flow.library.documentsCount', { count: documents.value.length }),
    }
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
  return segments.length > 1 ? (segments.at(-1) ?? '') : ''
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

function classifyUploadKind(file: File): UploadFileKind {
  const extension = getFileExtension(file.name)

  if (extension === 'pdf' || file.type === 'application/pdf') {
    return 'pdf'
  }

  if (file.type.startsWith('image/') || imageExtensions.has(extension)) {
    return 'image'
  }

  if (isTextLikeUpload(file)) {
    return 'text_like'
  }

  return 'binary'
}

function describeUploadSelection(file: File): UploadSelectionState {
  const fileKind = classifyUploadKind(file)

  switch (fileKind) {
    case 'text_like':
      return {
        supportStatus: 'supported_now',
        fileKind,
        fileKindLabel: t('flow.library.upload.selection.textLikeKind'),
        badgeLabel: t('flow.library.upload.selection.readyLabel'),
        badgeTone: 'positive',
        bannerTone: 'success',
        message: t('flow.library.upload.selection.textLike'),
      }
    case 'pdf':
      return {
        supportStatus: 'planned',
        fileKind,
        fileKindLabel: t('flow.library.upload.selection.pdfKind'),
        badgeLabel: t('flow.library.upload.selection.plannedLabel'),
        badgeTone: 'warning',
        bannerTone: 'warning',
        message: t('flow.library.upload.selection.pdfPlanned'),
      }
    case 'image':
      return {
        supportStatus: 'planned',
        fileKind,
        fileKindLabel: t('flow.library.upload.selection.imageKind'),
        badgeLabel: t('flow.library.upload.selection.plannedLabel'),
        badgeTone: 'warning',
        bannerTone: 'warning',
        message: t('flow.library.upload.selection.imagePlanned'),
      }
    default:
      return {
        supportStatus: 'unsupported',
        fileKind,
        fileKindLabel: t('flow.library.upload.selection.binaryKind'),
        badgeLabel: t('flow.library.upload.selection.unsupportedLabel'),
        badgeTone: 'negative',
        bannerTone: 'danger',
        message: t('flow.library.upload.selection.binaryUnsupported'),
      }
  }
}

function formatFileSize(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unitIndex = 0

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex += 1
  }

  const precision = unitIndex === 0 ? 0 : 1
  return `${value.toFixed(precision)} ${units[unitIndex]}`
}

const uploadSelection = computed(() =>
  uploadFile.value ? describeUploadSelection(uploadFile.value) : null,
)
const canUploadSelectedFile = computed(() =>
  Boolean(
    selectedProjectId.value &&
    uploadFile.value &&
    uploadSelection.value?.supportStatus === 'supported_now' &&
    !loading.value,
  ),
)

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

function clearSelectedUpload() {
  uploadFile.value = null
  uploadTitle.value = ''
  uploadInputKey.value += 1
  isUploadDragActive.value = false
}

function setUploadFile(file: File | null) {
  uploadFile.value = file
  isUploadDragActive.value = false
  statusMessage.value = null
  errorMessage.value = null

  if (file && !uploadTitle.value.trim()) {
    uploadTitle.value = file.name
  }
}

function openUploadPicker() {
  uploadInputRef.value?.click()
}

function handleUploadFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  setUploadFile(input.files?.[0] ?? null)
}

function handleUploadDragEnter() {
  isUploadDragActive.value = true
}

function handleUploadDragOver(event: DragEvent) {
  event.preventDefault()
  isUploadDragActive.value = true
  if (event.dataTransfer) {
    event.dataTransfer.dropEffect = 'copy'
  }
}

function handleUploadDragLeave(event: DragEvent) {
  const currentTarget = event.currentTarget as HTMLElement | null
  const relatedTarget = event.relatedTarget as Node | null
  if (currentTarget?.contains(relatedTarget)) {
    return
  }

  isUploadDragActive.value = false
}

function handleUploadDrop(event: DragEvent) {
  event.preventDefault()
  setUploadFile(event.dataTransfer?.files?.[0] ?? null)
}

function formatRunStartedMessage(jobId: string) {
  return `Run ${jobId} started. Waiting for completion.`
}

function formatRunCompletedMessage(jobId: string) {
  return `Run ${jobId} completed. Content is indexed.`
}

function formatRunProgressMessage(jobId: string, status: string, stage: string) {
  return `Run ${jobId}: ${status} at ${stage}.`
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
    errorMessage.value = 'Choose a collection in Setup first.'
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

    statusMessage.value = formatRunStartedMessage(result.ingestion_job_id)

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)
    if (jobDetail?.status === 'completed') {
      statusMessage.value = formatRunCompletedMessage(jobDetail.id)
      text.value = ''
      title.value = ''
      externalKey.value = `note-${String(Date.now())}`
      return
    }

    if (jobDetail?.error_message) {
      errorMessage.value = `Run ${jobDetail.id} failed: ${jobDetail.error_message}`
      statusMessage.value = null
      return
    }

    const status = jobDetail?.status ?? result.status
    const stage = jobDetail?.stage ?? result.stage
    statusMessage.value = formatRunProgressMessage(result.ingestion_job_id, status, stage)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Indexing failed'
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
    errorMessage.value = 'Choose a collection in Setup first.'
    loading.value = false
    return
  }

  if (!uploadFile.value) {
    errorMessage.value = 'Choose a file first.'
    loading.value = false
    return
  }

  const selection = uploadSelection.value
  if (!selection || selection.supportStatus !== 'supported_now') {
    errorMessage.value =
      selection?.message ??
      (isBlockedBinaryUpload(uploadFile.value)
        ? t('flow.library.upload.blockedError')
        : t('flow.library.upload.unsupportedError'))
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

    statusMessage.value = formatRunStartedMessage(result.ingestion_job_id)

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)
    if (jobDetail?.status === 'completed') {
      statusMessage.value = formatRunCompletedMessage(jobDetail.id)
      clearSelectedUpload()
      return
    }

    if (jobDetail?.error_message) {
      errorMessage.value = `Run ${jobDetail.id} failed: ${jobDetail.error_message}`
      statusMessage.value = null
      return
    }

    const status = jobDetail?.status ?? result.status
    const stage = jobDetail?.stage ?? result.stage
    statusMessage.value = formatRunProgressMessage(result.ingestion_job_id, status, stage)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Upload failed'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="rr-page-grid ingestion-page">
    <PageSection
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
                :label="
                  selectedProjectId
                    ? t('flow.library.form.ready')
                    : t('flow.library.form.needsSetup')
                "
              />
            </div>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.sourceLabel') }}</span>
                <input
                  v-model="sourceLabel"
                  class="rr-control"
                  type="text"
                  placeholder="Pasted text"
                />
              </label>
              <div class="rr-form-grid rr-form-grid--two">
                <label class="rr-field">
                  <span class="rr-field__label">{{ t('flow.library.form.externalKey') }}</span>
                  <input
                    v-model="externalKey"
                    class="rr-control"
                    type="text"
                    placeholder="note-001"
                  />
                </label>
                <label class="rr-field">
                  <span class="rr-field__label">{{ t('flow.library.form.titleLabel') }}</span>
                  <input
                    v-model="title"
                    class="rr-control"
                    type="text"
                    placeholder="Support policy"
                  />
                </label>
              </div>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.text') }}</span>
                <textarea
                  v-model="text"
                  class="rr-control"
                  rows="12"
                  placeholder="Paste content to index"
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
                :label="
                  selectedProjectId
                    ? t('flow.library.upload.ready')
                    : t('flow.library.upload.needsSetup')
                "
              />
            </div>

            <p class="rr-banner" data-tone="info">
              {{ t('flow.library.upload.hintCompact') }}
            </p>

            <div
              class="upload-dropzone"
              :class="{ 'is-active': isUploadDragActive, 'is-selected': !!uploadFile }"
              @click="openUploadPicker"
              @dragenter.prevent="handleUploadDragEnter"
              @dragover.prevent="handleUploadDragOver"
              @dragleave.prevent="handleUploadDragLeave"
              @drop.prevent="handleUploadDrop"
            >
              <input
                :key="uploadInputKey"
                ref="uploadInputRef"
                class="upload-dropzone__input"
                type="file"
                :accept="acceptedUploadTypes"
                @change="handleUploadFileChange"
              />

              <div class="upload-dropzone__body">
                <StatusBadge tone="info" :label="t('flow.library.upload.dropzoneIdleBadge')" />
                <h4>
                  {{
                    isUploadDragActive
                      ? t('flow.library.upload.dropzoneActiveTitle')
                      : t('flow.library.upload.dropzoneTitle')
                  }}
                </h4>
                <p>
                  {{
                    isUploadDragActive
                      ? t('flow.library.upload.dropzoneActiveBody')
                      : t('flow.library.upload.dropzoneBody')
                  }}
                </p>
                <button
                  type="button"
                  class="rr-button rr-button--secondary"
                  :disabled="loading"
                  @click="openUploadPicker"
                >
                  {{ t('flow.library.upload.browse') }}
                </button>
              </div>
            </div>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.upload.sourceLabel') }}</span>
                <input
                  v-model="uploadSourceLabel"
                  class="rr-control"
                  type="text"
                  placeholder="Uploaded file"
                />
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.upload.titleLabel') }}</span>
                <input
                  v-model="uploadTitle"
                  class="rr-control"
                  type="text"
                  placeholder="Article title"
                />
              </label>
            </div>

            <div v-if="uploadFile && uploadSelection" class="upload-selection-card">
              <div class="upload-selection-card__meta">
                <strong>{{ uploadFile.name }}</strong>
                <span class="rr-muted">
                  {{ uploadSelection.fileKindLabel }} · {{ formatFileSize(uploadFile.size) }}
                </span>
              </div>
              <StatusBadge :tone="uploadSelection.badgeTone" :label="uploadSelection.badgeLabel" />
            </div>

            <p v-if="uploadSelection" class="rr-banner" :data-tone="uploadSelection.bannerTone">
              {{ uploadSelection.message }}
            </p>

            <div class="rr-action-row">
              <button
                type="button"
                class="rr-button"
                :disabled="!canUploadSelectedFile"
                @click="uploadCurrentFile"
              >
                {{
                  loading ? t('flow.library.upload.actionBusy') : t('flow.library.upload.action')
                }}
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
                :label="
                  documents.length
                    ? t('flow.library.lists.documents.ready')
                    : t('flow.library.lists.documents.empty')
                "
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
                :label="
                  sources.length
                    ? t('flow.library.lists.sources.ready')
                    : t('flow.library.lists.sources.empty')
                "
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
              Run {{ latestJob.id }} is {{ latestJob.status }}.
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

.upload-dropzone {
  position: relative;
  overflow: hidden;
  cursor: pointer;
  border: 1.5px dashed rgb(15 23 42 / 0.16);
  border-radius: var(--rr-radius-xl);
  background:
    radial-gradient(circle at top right, rgb(59 130 246 / 0.14), transparent 42%),
    linear-gradient(160deg, rgb(255 255 255 / 0.96), rgb(241 245 249 / 0.88));
  transition:
    border-color 160ms ease,
    transform 160ms ease,
    box-shadow 160ms ease;
}

.upload-dropzone.is-active {
  border-color: var(--rr-color-accent-700);
  box-shadow: 0 18px 45px rgb(59 130 246 / 0.15);
  transform: translateY(-1px);
}

.upload-dropzone.is-selected {
  border-color: rgb(15 23 42 / 0.28);
}

.upload-dropzone__input {
  display: none;
}

.upload-dropzone__body {
  display: grid;
  justify-items: start;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
}

.upload-dropzone__body h4,
.upload-dropzone__body p {
  margin: 0;
}

.upload-dropzone__body p {
  max-width: 42rem;
  color: var(--rr-color-text-secondary);
}

.upload-selection-card {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
  padding: var(--rr-space-3);
  border-radius: var(--rr-radius-lg);
  border: 1px solid var(--rr-color-border-subtle);
  background: var(--rr-color-bg-surface-muted);
}

.upload-selection-card__meta {
  display: grid;
  gap: 6px;
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

  .upload-selection-card {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
