<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  createSource,
  fetchDocuments,
  fetchIngestionJobDetail,
  fetchIngestionJobs,
  fetchProjects,
  fetchSources,
  fetchWorkspaces,
  ingestText,
  retryIngestionJob,
  uploadAndIngest,
  type DocumentSummary,
  type IngestionJobDetail,
  type IngestionJobSummary,
  type SourceSummary,
} from 'src/boot/api'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import {
  buildJobSteps,
  describeIngestionError,
  describeIngestionJob,
  formatSourceKind,
  formatTriggerKind,
  isActiveJobStatus,
  isTerminalJobStatus,
  jobDetailFromSummary,
  shortJobId,
} from 'src/pages/support/ingestion-status'
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

interface FeedbackState {
  tone: 'success' | 'warning' | 'danger' | 'info'
  title: string
  body: string
  detail?: string
}

interface JobViewModel {
  job: IngestionJobDetail
  sourceLabel: string
  triggerLabel: string
  shortId: string
  presentation: ReturnType<typeof describeIngestionJob>
  error: ReturnType<typeof describeIngestionError> | null
  startedLabel: string | null
  updatedLabel: string | null
  durationLabel: string | null
}

const MAX_VISIBLE_QUEUE_ITEMS = 6
const POLL_INTERVAL_MS = 900
const MAX_POLL_ATTEMPTS = 12
const AUTO_REFRESH_INTERVAL_MS = 3000
const MANUAL_SOURCE_KIND = 'text'
const FILE_SOURCE_KIND = 'upload'

const { t } = useI18n()

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const documents = ref<DocumentSummary[]>([])
const sources = ref<SourceSummary[]>([])
const title = ref('')
const text = ref('')
const uploadTitle = ref('')
const uploadFile = ref<File | null>(null)
const uploadInputRef = ref<HTMLInputElement | null>(null)
const uploadInputKey = ref(0)
const isUploadDragActive = ref(false)
const feedback = ref<FeedbackState | null>(null)
const submitMode = ref<'text' | 'upload' | null>(null)
const retryingJobId = ref<string | null>(null)
const recentJobs = ref<IngestionJobDetail[]>([])
const queueLoading = ref(false)
let refreshTimer: number | null = null

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
const sourceLabelById = computed(
  () => new Map(sources.value.map((item) => [item.id, item.label])),
)

const activeJobsCount = computed(
  () => recentJobs.value.filter((job) => isActiveJobStatus(job.status)).length,
)
const uploadSelection = computed(() =>
  uploadFile.value ? describeUploadSelection(uploadFile.value) : null,
)
const canUploadSelectedFile = computed(() =>
  Boolean(
    selectedProjectId.value &&
      uploadFile.value &&
      uploadSelection.value?.supportStatus === 'supported_now' &&
      submitMode.value !== 'upload',
  ),
)
const pageStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: t('flow.library.statusBlocked') }
  }

  if (activeJobsCount.value > 0) {
    return {
      status: 'partial',
      label: t('flow.library.statusProcessing', { count: activeJobsCount.value }),
    }
  }

  if (recentJobs.value[0] && ['failed', 'retryable_failed', 'canceled'].includes(recentJobs.value[0].status)) {
    return {
      status: 'warning',
      label: t('flow.library.statusAttention'),
    }
  }

  if (documents.value.length > 0) {
    return {
      status: 'ready',
      label: t('flow.library.documentsCount', { count: documents.value.length }),
    }
  }

  return { status: 'draft', label: t('flow.library.statusDraft') }
})
const highlightedJob = computed(
  () => recentJobs.value.find((job) => isActiveJobStatus(job.status)) ?? recentJobs.value[0] ?? null,
)
const jobViewModels = computed<JobViewModel[]>(() =>
  recentJobs.value.map((job) => ({
    job,
    sourceLabel:
      (job.source_id ? sourceLabelById.value.get(job.source_id) : null) ??
      formatTriggerKind(job.trigger_kind, t),
    triggerLabel: formatTriggerKind(job.trigger_kind, t),
    shortId: shortJobId(job.id),
    presentation: describeIngestionJob(job, t),
    error: job.error_message ? describeIngestionError(job.error_message, t) : null,
    startedLabel: formatDateTime(job.started_at),
    updatedLabel: formatDateTime(job.finished_at ?? job.started_at),
    durationLabel: formatDuration(job.started_at, job.finished_at),
  })),
)
const highlightedJobView = computed(
  () => jobViewModels.value.find((item) => item.job.id === highlightedJob.value?.id) ?? null,
)
const highlightedJobSteps = computed(() =>
  highlightedJob.value ? buildJobSteps(highlightedJob.value, t) : [],
)
const visibleDocuments = computed(() => documents.value.slice(0, MAX_VISIBLE_QUEUE_ITEMS))
const visibleSources = computed(() => sources.value.slice(0, MAX_VISIBLE_QUEUE_ITEMS))
const remainingDocumentCount = computed(() =>
  Math.max(0, documents.value.length - MAX_VISIBLE_QUEUE_ITEMS),
)
const remainingSourceCount = computed(() =>
  Math.max(0, sources.value.length - MAX_VISIBLE_QUEUE_ITEMS),
)
const processingStatLabel = computed(() => {
  if (activeJobsCount.value > 0) {
    return t('flow.library.stats.processingActive', { count: activeJobsCount.value })
  }

  if (highlightedJobView.value) {
    return highlightedJobView.value.presentation.statusLabel
  }

  return t('flow.library.stats.processingIdle')
})
const processingStatHint = computed(() => {
  if (highlightedJobView.value) {
    return highlightedJobView.value.presentation.stageLabel
  }

  return t('flow.library.stats.processingHint')
})

function slugify(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 48)
}

function buildExternalKey(prefix: string, seed: string): string {
  const base = slugify(seed) || prefix
  return `${prefix}-${base}-${Date.now()}`
}

function setFeedbackState(state: FeedbackState | null) {
  feedback.value = state
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

function formatDateTime(value?: string | null): string | null {
  if (!value) {
    return null
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return null
  }

  return new Intl.DateTimeFormat(undefined, {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

function formatDuration(startedAt?: string | null, finishedAt?: string | null): string | null {
  if (!startedAt) {
    return null
  }

  const started = new Date(startedAt).getTime()
  const finished = finishedAt ? new Date(finishedAt).getTime() : Date.now()

  if (Number.isNaN(started) || Number.isNaN(finished) || finished < started) {
    return null
  }

  const totalSeconds = Math.round((finished - started) / 1000)
  if (totalSeconds < 60) {
    return `${totalSeconds}s`
  }

  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  if (minutes < 60) {
    return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`
  }

  const hours = Math.floor(minutes / 60)
  const remainingMinutes = minutes % 60
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`
}

function formatDocumentStatus(status?: string | null): string {
  if (!status) {
    return t('flow.library.lists.documents.indexed')
  }

  return status
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (char) => char.toUpperCase())
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms)
  })
}

function stopAutoRefresh() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function startAutoRefresh() {
  if (refreshTimer !== null || !selectedProjectId.value) {
    return
  }

  refreshTimer = window.setInterval(() => {
    if (!selectedProjectId.value || queueLoading.value) {
      return
    }

    void refreshProcessingState(false)
  }, AUTO_REFRESH_INTERVAL_MS)
}

watch(
  activeJobsCount,
  (count) => {
    if (count > 0) {
      startAutoRefresh()
      return
    }

    stopAutoRefresh()
  },
  { immediate: true },
)

async function hydrateRecentJobs(jobSummaries: IngestionJobSummary[]) {
  queueLoading.value = true

  try {
    const details = await Promise.all(
      jobSummaries.slice(0, MAX_VISIBLE_QUEUE_ITEMS).map(async (summary) => {
        try {
          return await fetchIngestionJobDetail(summary.id)
        } catch {
          return jobDetailFromSummary(summary)
        }
      }),
    )
    recentJobs.value = details
  } finally {
    queueLoading.value = false
  }
}

async function loadProjectData(projectId: string) {
  const [docs, srcs, jobs] = await Promise.all([
    fetchDocuments(projectId),
    fetchSources(projectId),
    fetchIngestionJobs(projectId),
  ])

  documents.value = docs
  sources.value = srcs
  await hydrateRecentJobs(jobs)
}

async function refreshProcessingState(showConfirmation: boolean) {
  if (!selectedProjectId.value) {
    return
  }

  try {
    await loadProjectData(selectedProjectId.value)
    if (showConfirmation) {
      setFeedbackState({
        tone: 'info',
        title: t('flow.library.notices.refreshedTitle'),
        body: t('flow.library.notices.refreshedBody'),
      })
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : t('flow.library.notices.genericErrorBody')
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
  }
}

function upsertJob(job: IngestionJobDetail) {
  recentJobs.value = [job, ...recentJobs.value.filter((item) => item.id !== job.id)].slice(
    0,
    MAX_VISIBLE_QUEUE_ITEMS,
  )
}

async function waitForIngestionJob(jobId: string): Promise<IngestionJobDetail | null> {
  for (let attempt = 0; attempt < MAX_POLL_ATTEMPTS; attempt += 1) {
    const detail = await fetchIngestionJobDetail(jobId)
    upsertJob(detail)

    if (isTerminalJobStatus(detail.status)) {
      return detail
    }

    await sleep(POLL_INTERVAL_MS)
  }

  return recentJobs.value.find((item) => item.id === jobId) ?? null
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
  setFeedbackState(null)

  if (file && !uploadTitle.value.trim()) {
    uploadTitle.value = file.name
  }
}

function openUploadPicker(event?: Event) {
  event?.stopPropagation()
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

function getAutoSourceLabel(sourceKind: string): string {
  return sourceKind === FILE_SOURCE_KIND
    ? t('flow.library.upload.autoSourceLabel')
    : t('flow.library.form.autoSourceLabel')
}

async function ensureSource(sourceKind: string): Promise<string> {
  const existing = sources.value.find((item) => item.source_kind === sourceKind)
  if (existing) {
    return existing.id
  }

  const source = await createSource({
    project_id: selectedProjectId.value!,
    source_kind: sourceKind,
    label: getAutoSourceLabel(sourceKind),
  })
  sources.value = [source, ...sources.value.filter((item) => item.id !== source.id)]
  return source.id
}

async function ingestCurrentText() {
  if (!selectedProjectId.value) {
    setFeedbackState({
      tone: 'danger',
      title: t('flow.library.notices.collectionTitle'),
      body: t('flow.library.notices.collectionBody'),
    })
    return
  }

  if (!text.value.trim()) {
    setFeedbackState({
      tone: 'danger',
      title: t('flow.library.notices.emptyTitle'),
      body: t('flow.library.notices.emptyBody'),
    })
    return
  }

  submitMode.value = 'text'
  setFeedbackState(null)

  try {
    const sourceId = await ensureSource(MANUAL_SOURCE_KIND)
    const externalKey = buildExternalKey('note', title.value || text.value.slice(0, 48))
    const result = await ingestText({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      external_key: externalKey,
      title: title.value.trim() || null,
      text: text.value,
    })

    setFeedbackState({
      tone: 'info',
      title: t('flow.library.notices.queuedTitle'),
      body: t('flow.library.notices.queuedBody'),
      detail: `${t('flow.library.processing.runId')}: ${shortJobId(result.ingestion_job_id)}`,
    })

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)

    if (jobDetail?.status === 'completed') {
      setFeedbackState({
        tone: 'success',
        title: t('flow.library.notices.completedTitle'),
        body: t('flow.library.notices.completedBody'),
        detail: `${t('flow.library.processing.runId')}: ${shortJobId(jobDetail.id)}`,
      })
      title.value = ''
      text.value = ''
      return
    }

    if (jobDetail?.error_message) {
      const copy = describeIngestionError(jobDetail.error_message, t)
      setFeedbackState({
        tone: 'danger',
        title: copy.title,
        body: copy.body,
        detail: copy.detail,
      })
      return
    }

    setFeedbackState({
      tone: 'warning',
      title: t('flow.library.notices.progressTitle'),
      body: t('flow.library.notices.progressBody'),
      detail:
        highlightedJobView.value?.presentation.stageLabel ??
        t('flow.library.processing.stages.unknown'),
    })
  } catch (error) {
    const message = error instanceof Error ? error.message : t('flow.library.notices.genericErrorBody')
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
  } finally {
    submitMode.value = null
  }
}

async function uploadCurrentFile() {
  if (!selectedProjectId.value) {
    setFeedbackState({
      tone: 'danger',
      title: t('flow.library.notices.collectionTitle'),
      body: t('flow.library.notices.collectionBody'),
    })
    return
  }

  if (!uploadFile.value) {
    setFeedbackState({
      tone: 'danger',
      title: t('flow.library.notices.fileTitle'),
      body: t('flow.library.notices.fileBody'),
    })
    return
  }

  const selection = uploadSelection.value
  if (!selection || selection.supportStatus !== 'supported_now') {
    const message =
      selection?.message ??
      (isBlockedBinaryUpload(uploadFile.value)
        ? t('flow.library.upload.blockedError')
        : t('flow.library.upload.unsupportedError'))
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
    return
  }

  submitMode.value = 'upload'
  setFeedbackState(null)

  try {
    const sourceId = await ensureSource(FILE_SOURCE_KIND)
    const result = await uploadAndIngest({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      title: uploadTitle.value.trim() || null,
      file: uploadFile.value,
    })

    setFeedbackState({
      tone: 'info',
      title: t('flow.library.notices.queuedTitle'),
      body: t('flow.library.notices.queuedBody'),
      detail: `${t('flow.library.processing.runId')}: ${shortJobId(result.ingestion_job_id)}`,
    })

    const jobDetail = await waitForIngestionJob(result.ingestion_job_id)
    await loadProjectData(selectedProjectId.value)

    if (jobDetail?.status === 'completed') {
      setFeedbackState({
        tone: 'success',
        title: t('flow.library.notices.completedTitle'),
        body: t('flow.library.notices.completedBody'),
        detail: `${t('flow.library.processing.runId')}: ${shortJobId(jobDetail.id)}`,
      })
      clearSelectedUpload()
      return
    }

    if (jobDetail?.error_message) {
      const copy = describeIngestionError(jobDetail.error_message, t)
      setFeedbackState({
        tone: 'danger',
        title: copy.title,
        body: copy.body,
        detail: copy.detail,
      })
      return
    }

    setFeedbackState({
      tone: 'warning',
      title: t('flow.library.notices.progressTitle'),
      body: t('flow.library.notices.progressBody'),
      detail:
        highlightedJobView.value?.presentation.stageLabel ??
        t('flow.library.processing.stages.unknown'),
    })
  } catch (error) {
    const message = error instanceof Error ? error.message : t('flow.library.notices.genericErrorBody')
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
  } finally {
    submitMode.value = null
  }
}

async function retryJob(jobId: string) {
  if (!selectedProjectId.value) {
    return
  }

  retryingJobId.value = jobId
  setFeedbackState(null)

  try {
    const retried = await retryIngestionJob(jobId)
    upsertJob(retried)
    setFeedbackState({
      tone: 'info',
      title: t('flow.library.notices.retryQueuedTitle'),
      body: t('flow.library.notices.retryQueuedBody'),
      detail: `${t('flow.library.processing.runId')}: ${shortJobId(retried.id)}`,
    })

    const terminalState = await waitForIngestionJob(retried.id)
    await loadProjectData(selectedProjectId.value)

    if (terminalState?.status === 'completed') {
      setFeedbackState({
        tone: 'success',
        title: t('flow.library.notices.completedTitle'),
        body: t('flow.library.notices.completedBody'),
        detail: `${t('flow.library.processing.runId')}: ${shortJobId(terminalState.id)}`,
      })
      return
    }

    if (terminalState?.error_message) {
      const copy = describeIngestionError(terminalState.error_message, t)
      setFeedbackState({
        tone: 'danger',
        title: copy.title,
        body: copy.body,
        detail: copy.detail,
      })
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : t('flow.library.notices.genericErrorBody')
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
  } finally {
    retryingJobId.value = null
  }
}

onMounted(async () => {
  try {
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
  } catch (error) {
    const message = error instanceof Error ? error.message : t('flow.library.notices.genericErrorBody')
    const copy = describeIngestionError(message, t)
    setFeedbackState({
      tone: 'danger',
      title: copy.title,
      body: copy.body,
      detail: copy.detail,
    })
  }
})

onUnmounted(() => {
  stopAutoRefresh()
})
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
        <button
          type="button"
          class="rr-button rr-button--secondary"
          :disabled="queueLoading"
          @click="refreshProcessingState(true)"
        >
          {{ t('flow.library.processing.refresh') }}
        </button>
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
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.library.stats.processing') }}</p>
          <strong>{{ processingStatLabel }}</strong>
          <p>{{ processingStatHint }}</p>
        </article>
      </div>

      <article
        v-if="feedback"
        class="feedback-banner"
        :data-tone="feedback.tone"
      >
        <strong>{{ feedback.title }}</strong>
        <p>{{ feedback.body }}</p>
        <p
          v-if="feedback.detail"
          class="feedback-banner__detail"
        >
          {{ feedback.detail }}
        </p>
      </article>

      <div class="ingestion-grid">
        <div class="ingestion-primary rr-grid">
          <article class="rr-panel rr-panel--accent rr-stack processing-overview">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.processing.kicker') }}</p>
                <h3>
                  {{
                    activeJobsCount > 0
                      ? t('flow.library.processing.activeTitle')
                      : highlightedJobView
                        ? t('flow.library.processing.latestTitle')
                        : t('flow.library.processing.emptyTitle')
                  }}
                </h3>
              </div>
              <StatusBadge
                v-if="highlightedJobView"
                :tone="highlightedJobView.presentation.tone"
                :label="highlightedJobView.presentation.statusLabel"
                emphasis="strong"
              />
              <StatusBadge
                v-else
                tone="info"
                :label="t('flow.library.processing.queueIdle')"
                emphasis="strong"
              />
            </div>

            <p class="rr-note">
              {{
                highlightedJobView
                  ? highlightedJobView.presentation.summary
                  : t('flow.library.processing.emptyBody')
              }}
            </p>

            <div
              v-if="highlightedJobView"
              class="processing-meta"
            >
              <article class="processing-meta__card">
                <span>{{ t('flow.library.processing.currentSource') }}</span>
                <strong>{{ highlightedJobView.sourceLabel }}</strong>
              </article>
              <article class="processing-meta__card">
                <span>{{ t('flow.library.processing.currentTrigger') }}</span>
                <strong>{{ highlightedJobView.triggerLabel }}</strong>
              </article>
              <article class="processing-meta__card">
                <span>{{ t('flow.library.processing.currentUpdated') }}</span>
                <strong>{{
                  highlightedJobView.updatedLabel ?? t('flow.library.processing.updating')
                }}</strong>
              </article>
              <article class="processing-meta__card">
                <span>{{ t('flow.library.processing.currentDuration') }}</span>
                <strong>{{
                  highlightedJobView.durationLabel ?? t('flow.library.processing.notStarted')
                }}</strong>
              </article>
            </div>

            <div
              v-if="highlightedJobSteps.length"
              class="processing-steps"
            >
              <article
                v-for="step in highlightedJobSteps"
                :key="step.key"
                class="processing-step"
                :data-state="step.state"
              >
                <div class="processing-step__dot" />
                <div class="processing-step__copy">
                  <strong>{{ step.label }}</strong>
                  <p>{{ step.description }}</p>
                </div>
              </article>
            </div>

            <article
              v-if="highlightedJobView?.error"
              class="processing-error"
            >
              <strong>{{ highlightedJobView.error.title }}</strong>
              <p>{{ highlightedJobView.error.body }}</p>
              <p
                v-if="highlightedJobView.error.detail"
                class="processing-error__detail"
              >
                {{ highlightedJobView.error.detail }}
              </p>
            </article>

            <div
              v-if="highlightedJobView"
              class="rr-action-row"
            >
              <button
                type="button"
                class="rr-button rr-button--secondary"
                :disabled="queueLoading"
                @click="refreshProcessingState(true)"
              >
                {{ t('flow.library.processing.refresh') }}
              </button>
              <button
                v-if="highlightedJobView.job.retryable"
                type="button"
                class="rr-button"
                :disabled="retryingJobId === highlightedJobView.job.id"
                @click="retryJob(highlightedJobView.job.id)"
              >
                {{
                  retryingJobId === highlightedJobView.job.id
                    ? t('flow.library.processing.retryBusy')
                    : t('flow.library.processing.retryAction')
                }}
              </button>
            </div>
          </article>

          <article class="rr-panel rr-stack">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.form.kicker') }}</p>
                <h3>{{ t('flow.library.form.title') }}</h3>
              </div>
              <StatusBadge
                :status="selectedProjectId ? 'ready' : 'blocked'"
                :label="
                  selectedProjectId
                    ? t('flow.library.form.ready')
                    : t('flow.library.form.needsSetup')
                "
              />
            </div>

            <p class="rr-note">{{ t('flow.library.form.helper') }}</p>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.titleLabel') }}</span>
                <input
                  v-model="title"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.library.form.titlePlaceholder')"
                />
                <p class="rr-field__hint">{{ t('flow.library.form.titleHint') }}</p>
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.library.form.text') }}</span>
                <textarea
                  v-model="text"
                  class="rr-control"
                  rows="12"
                  :placeholder="t('flow.library.form.textPlaceholder')"
                />
                <p class="rr-field__hint">{{ t('flow.library.form.autoHint') }}</p>
              </label>
            </div>

            <div class="rr-action-row">
              <button
                type="button"
                class="rr-button"
                :disabled="!selectedProjectId || !text.trim() || submitMode === 'text'"
                @click="ingestCurrentText"
              >
                {{
                  submitMode === 'text'
                    ? t('flow.library.form.actionBusy')
                    : t('flow.library.form.action')
                }}
              </button>
            </div>
          </article>

          <article class="rr-panel rr-stack">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.upload.kicker') }}</p>
                <h3>{{ t('flow.library.upload.title') }}</h3>
              </div>
              <StatusBadge
                :status="selectedProjectId ? 'ready' : 'blocked'"
                :label="
                  selectedProjectId
                    ? t('flow.library.upload.ready')
                    : t('flow.library.upload.needsSetup')
                "
              />
            </div>

            <p class="rr-note">{{ t('flow.library.upload.helper') }}</p>

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
                  :disabled="submitMode === 'upload'"
                  @click.stop="openUploadPicker"
                >
                  {{ t('flow.library.upload.browse') }}
                </button>
              </div>
            </div>

            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.library.upload.titleLabel') }}</span>
              <input
                v-model="uploadTitle"
                class="rr-control"
                type="text"
                :placeholder="t('flow.library.upload.titlePlaceholder')"
              />
              <p class="rr-field__hint">{{ t('flow.library.upload.titleHint') }}</p>
            </label>

            <div
              v-if="uploadFile && uploadSelection"
              class="upload-selection-card"
            >
              <div class="upload-selection-card__meta">
                <strong>{{ uploadFile.name }}</strong>
                <span class="rr-muted">
                  {{ uploadSelection.fileKindLabel }} · {{ formatFileSize(uploadFile.size) }}
                </span>
              </div>
              <StatusBadge :tone="uploadSelection.badgeTone" :label="uploadSelection.badgeLabel" />
            </div>

            <p
              v-if="uploadSelection"
              class="rr-banner"
              :data-tone="uploadSelection.bannerTone"
            >
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
                  submitMode === 'upload'
                    ? t('flow.library.upload.actionBusy')
                    : t('flow.library.upload.action')
                }}
              </button>
            </div>
          </article>
        </div>

        <div class="ingestion-side rr-grid">
          <article class="rr-panel rr-panel--muted rr-stack">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.processing.queueKicker') }}</p>
                <h3>{{ t('flow.library.processing.queueTitle') }}</h3>
              </div>
              <StatusBadge
                :status="activeJobsCount > 0 ? 'running' : recentJobs.length ? 'ready' : 'draft'"
                :label="
                  activeJobsCount > 0
                    ? t('flow.library.processing.queueCount', { count: activeJobsCount })
                    : recentJobs.length
                      ? t('flow.library.processing.queueLoaded', { count: recentJobs.length })
                      : t('flow.library.processing.queueIdle')
                "
              />
            </div>

            <EmptyStateCard
              v-if="!recentJobs.length && !queueLoading"
              :title="t('flow.library.processing.emptyTitle')"
              :message="t('flow.library.processing.emptyBody')"
            />

            <ul
              v-else
              class="job-queue"
            >
              <li
                v-for="item in jobViewModels"
                :key="item.job.id"
                class="job-queue__item"
              >
                <div class="job-queue__header">
                  <div>
                    <strong>{{ item.sourceLabel }}</strong>
                    <p class="rr-muted">
                      {{ item.triggerLabel }} · {{ t('flow.library.processing.runId') }}
                      {{ item.shortId }}
                    </p>
                  </div>
                  <StatusBadge
                    :tone="item.presentation.tone"
                    :label="item.presentation.statusLabel"
                  />
                </div>

                <p class="job-queue__summary">{{ item.presentation.summary }}</p>

                <div class="job-queue__meta">
                  <span>{{ item.presentation.stageLabel }}</span>
                  <span v-if="item.startedLabel">
                    {{ t('flow.library.processing.currentSubmitted') }}: {{ item.startedLabel }}
                  </span>
                  <span v-if="item.durationLabel">
                    {{ t('flow.library.processing.currentDuration') }}: {{ item.durationLabel }}
                  </span>
                </div>

                <p
                  v-if="item.error"
                  class="job-queue__error"
                >
                  {{ item.error.body }}
                </p>

                <div
                  v-if="item.job.retryable"
                  class="job-queue__actions"
                >
                  <button
                    type="button"
                    class="rr-button rr-button--secondary"
                    :disabled="retryingJobId === item.job.id"
                    @click="retryJob(item.job.id)"
                  >
                    {{
                      retryingJobId === item.job.id
                        ? t('flow.library.processing.retryBusy')
                        : t('flow.library.processing.retryAction')
                    }}
                  </button>
                </div>
              </li>
            </ul>
          </article>

          <article class="rr-panel rr-stack">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.lists.documents.kicker') }}</p>
                <h3>{{ t('flow.library.lists.documents.title') }}</h3>
              </div>
              <StatusBadge
                :status="documents.length ? 'ready' : 'draft'"
                :label="
                  documents.length
                    ? t('flow.library.lists.documents.ready')
                    : t('flow.library.lists.documents.empty')
                "
              />
            </div>

            <p
              v-if="!documents.length"
              class="rr-note"
            >
              {{ t('flow.library.lists.documents.emptyMessage') }}
            </p>

            <ul
              v-else
              class="inventory-list"
            >
              <li
                v-for="document in visibleDocuments"
                :key="document.id"
              >
                <div>
                  <strong>{{ document.title || document.external_key }}</strong>
                  <p class="rr-muted">{{ document.external_key }}</p>
                </div>
                <StatusBadge
                  tone="positive"
                  :label="formatDocumentStatus(document.status)"
                />
              </li>
            </ul>

            <p
              v-if="remainingDocumentCount > 0"
              class="rr-note"
            >
              {{ t('flow.library.lists.documents.more', { count: remainingDocumentCount }) }}
            </p>
          </article>

          <article class="rr-panel rr-panel--muted rr-stack">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.lists.sources.kicker') }}</p>
                <h3>{{ t('flow.library.lists.sources.title') }}</h3>
              </div>
              <StatusBadge
                :status="sources.length ? 'ready' : 'draft'"
                :label="
                  sources.length
                    ? t('flow.library.lists.sources.ready')
                    : t('flow.library.lists.sources.empty')
                "
              />
            </div>

            <p
              v-if="!sources.length"
              class="rr-note"
            >
              {{ t('flow.library.lists.sources.emptyMessage') }}
            </p>

            <ul
              v-else
              class="inventory-list"
            >
              <li
                v-for="source in visibleSources"
                :key="source.id"
              >
                <div>
                  <strong>{{ source.label }}</strong>
                  <p class="rr-muted">{{ formatSourceKind(source.source_kind, t) }}</p>
                </div>
                <StatusBadge :label="source.status" />
              </li>
            </ul>

            <p
              v-if="remainingSourceCount > 0"
              class="rr-note"
            >
              {{ t('flow.library.lists.sources.more', { count: remainingSourceCount }) }}
            </p>
          </article>
        </div>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.rr-stack--tight {
  gap: 0.35rem;
}

.ingestion-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.2fr) minmax(340px, 0.8fr);
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

.feedback-banner {
  display: grid;
  gap: 0.35rem;
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-lg);
  border: 1px solid transparent;
}

.feedback-banner strong,
.feedback-banner p {
  margin: 0;
}

.feedback-banner[data-tone='success'] {
  border-color: rgb(34 197 94 / 0.22);
  background: rgb(240 253 244 / 0.96);
  color: var(--rr-color-success-600);
}

.feedback-banner[data-tone='warning'] {
  border-color: rgb(245 158 11 / 0.24);
  background: rgb(255 251 235 / 0.98);
  color: var(--rr-color-warning-600);
}

.feedback-banner[data-tone='danger'] {
  border-color: rgb(239 68 68 / 0.24);
  background: rgb(254 242 242 / 0.98);
  color: var(--rr-color-danger-600);
}

.feedback-banner[data-tone='info'] {
  border-color: rgb(59 130 246 / 0.24);
  background: rgb(239 246 255 / 0.96);
  color: var(--rr-color-accent-700);
}

.feedback-banner__detail {
  font-size: 0.92rem;
  opacity: 0.85;
}

.processing-overview {
  background:
    radial-gradient(circle at top right, rgb(29 78 216 / 0.12), transparent 40%),
    linear-gradient(180deg, rgb(255 255 255 / 0.98), rgb(243 247 255 / 0.96)),
    var(--rr-color-bg-surface-strong);
}

.processing-meta {
  display: grid;
  gap: var(--rr-space-3);
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
}

.processing-meta__card {
  display: grid;
  gap: 0.35rem;
  padding: 0.95rem 1rem;
  border-radius: var(--rr-radius-md);
  border: 1px solid rgb(29 78 216 / 0.12);
  background: rgb(255 255 255 / 0.7);
}

.processing-meta__card span {
  font-size: 0.74rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.processing-meta__card strong {
  font-size: 0.96rem;
}

.processing-steps {
  display: grid;
  gap: var(--rr-space-3);
}

.processing-step {
  display: grid;
  grid-template-columns: 18px minmax(0, 1fr);
  gap: var(--rr-space-3);
  align-items: start;
  padding: 0.8rem 0.9rem;
  border-radius: var(--rr-radius-md);
  border: 1px solid var(--rr-color-border-subtle);
  background: rgb(255 255 255 / 0.72);
}

.processing-step__dot {
  width: 12px;
  height: 12px;
  margin-top: 0.3rem;
  border-radius: 999px;
  background: rgb(148 163 184 / 0.55);
}

.processing-step__copy {
  display: grid;
  gap: 0.2rem;
}

.processing-step__copy strong,
.processing-step__copy p {
  margin: 0;
}

.processing-step__copy p {
  color: var(--rr-color-text-secondary);
}

.processing-step[data-state='complete'] {
  border-color: rgb(34 197 94 / 0.18);
  background: rgb(240 253 244 / 0.72);
}

.processing-step[data-state='complete'] .processing-step__dot {
  background: var(--rr-color-success-600);
}

.processing-step[data-state='active'] {
  border-color: rgb(245 158 11 / 0.22);
  background: rgb(255 251 235 / 0.84);
}

.processing-step[data-state='active'] .processing-step__dot {
  background: var(--rr-color-warning-600);
}

.processing-step[data-state='error'] {
  border-color: rgb(239 68 68 / 0.24);
  background: rgb(254 242 242 / 0.82);
}

.processing-step[data-state='error'] .processing-step__dot {
  background: var(--rr-color-danger-600);
}

.processing-error {
  display: grid;
  gap: 0.35rem;
  padding: var(--rr-space-4);
  border: 1px solid rgb(239 68 68 / 0.2);
  border-radius: var(--rr-radius-md);
  background: rgb(254 242 242 / 0.86);
}

.processing-error strong,
.processing-error p {
  margin: 0;
}

.processing-error p {
  color: #7f1d1d;
}

.processing-error__detail {
  font-size: 0.92rem;
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

.job-queue {
  display: grid;
  gap: var(--rr-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.job-queue__item {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-md);
  border: 1px solid var(--rr-color-border-subtle);
  background: rgb(255 255 255 / 0.72);
}

.job-queue__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: start;
}

.job-queue__header strong,
.job-queue__header p,
.job-queue__summary,
.job-queue__error {
  margin: 0;
}

.job-queue__summary {
  color: var(--rr-color-text-secondary);
}

.job-queue__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0.65rem 1rem;
  font-size: 0.9rem;
  color: var(--rr-color-text-muted);
}

.job-queue__error {
  color: #991b1b;
}

.job-queue__actions {
  display: flex;
  justify-content: flex-start;
}

.inventory-list {
  display: grid;
  gap: var(--rr-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.inventory-list li {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: start;
  padding: 0.95rem 1rem;
  border-radius: var(--rr-radius-md);
  border: 1px solid var(--rr-color-border-subtle);
  background: rgb(255 255 255 / 0.72);
}

.inventory-list strong,
.inventory-list p {
  margin: 0;
}

@media (width <= 1100px) {
  .ingestion-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 700px) {
  .ingestion-panel__heading,
  .job-queue__header,
  .upload-selection-card,
  .inventory-list li {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
