<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute, useRouter } from 'vue-router'

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
import CrossSurfaceGuide from 'src/components/shell/CrossSurfaceGuide.vue'
import ProductSpine from 'src/components/shell/ProductSpine.vue'
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
  buildFileInventory,
  matchesInventoryFilter,
  type FileInventoryFilter,
} from 'src/pages/support/file-library'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  setWorkspaceWithProjectReset,
  syncWorkspaceProjectScope,
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
const route = useRoute()
const router = useRouter()

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
const libraryFilter = ref<FileInventoryFilter>('all')
const selectedDocumentId = ref<string | null>(null)
const librarySearch = ref('')
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
const sourceLabelById = computed(() => new Map(sources.value.map((item) => [item.id, item.label])))

const activeJobsCount = computed(() =>
  recentJobs.value.filter((job) => isActiveJobStatus(job.status)).length,
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

  if (
    recentJobs.value[0] &&
    ['failed', 'retryable_failed', 'canceled'].includes(recentJobs.value[0].status)
  ) {
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
const visibleSources = computed(() => sources.value.slice(0, MAX_VISIBLE_QUEUE_ITEMS))
const remainingSourceCount = computed(() => Math.max(0, sources.value.length - MAX_VISIBLE_QUEUE_ITEMS))
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
const inventoryFilterOptions = computed(() => [
  { value: 'all' as const, label: t('flow.library.inventory.filters.all') },
  { value: 'recent' as const, label: t('flow.library.inventory.filters.recent') },
  { value: 'attention' as const, label: t('flow.library.inventory.filters.attention') },
  { value: 'manual' as const, label: t('flow.library.inventory.filters.manual') },
  { value: 'upload' as const, label: t('flow.library.inventory.filters.upload') },
])
const fileInventory = computed(() =>
  buildFileInventory(documents.value, sources.value, recentJobs.value, {
    unknownSourceLabel: t('flow.library.inventory.unknownSource'),
    sourceKindFormatter: (value: string) => formatSourceKind(value, t),
    statusFormatter: (value?: string | null) => formatDocumentStatus(value),
    untitledLabel: t('flow.library.inventory.untitled'),
    recentLabel: t('flow.library.inventory.updatedPrefix'),
    checksumLabel: t('flow.library.inventory.checksum'),
    mimeFallback: t('flow.library.inventory.mimeFallback'),
  }),
)
const normalizedLibrarySearch = computed(() => librarySearch.value.trim().toLowerCase())
const filteredFileInventory = computed(() =>
  fileInventory.value
    .filter((record) => matchesInventoryFilter(record, libraryFilter.value))
    .filter((record) => {
      if (!normalizedLibrarySearch.value) {
        return true
      }

      const haystack = [
        record.title,
        record.subtitle,
        record.sourceLabel,
        record.sourceKindLabel,
        record.mimeLabel,
      ]
        .join(' ')
        .toLowerCase()

      return haystack.includes(normalizedLibrarySearch.value)
    })
    .sort((left, right) => {
      if (right.updatedSortValue !== left.updatedSortValue) {
        return right.updatedSortValue - left.updatedSortValue
      }

      return left.title.localeCompare(right.title)
    }),
)
const selectedInventoryRecord = computed(() => {
  const selectedFromFiltered = filteredFileInventory.value.find((item) => item.id === selectedDocumentId.value)
  if (selectedFromFiltered) {
    return selectedFromFiltered
  }

  const selectedFromAll = fileInventory.value.find((item) => item.id === selectedDocumentId.value)
  if (selectedFromAll) {
    return selectedFromAll
  }

  return filteredFileInventory.value[0] || fileInventory.value[0] || null
})
const selectedDocument = computed(() => {
  const selectedId = selectedInventoryRecord.value?.id
  return selectedId ? documents.value.find((item) => item.id === selectedId) ?? null : null
})
const selectedDocumentRelatedJob = computed(() => {
  const sourceId = selectedDocument.value?.source_id
  return sourceId ? recentJobs.value.find((job) => job.source_id === sourceId) ?? null : null
})
const selectedDocumentProcessingPresentation = computed(() =>
  selectedDocumentRelatedJob.value ? describeIngestionJob(selectedDocumentRelatedJob.value, t) : null,
)
const inventorySummary = computed(() => {
  const total = filteredFileInventory.value.length
  if (total === 0) {
    return t('flow.library.inventory.summaryEmpty')
  }

  const attention = filteredFileInventory.value.filter((item) => item.attention).length
  if (attention > 0) {
    return t('flow.library.inventory.summaryAttention', { count: attention, total })
  }

  return t('flow.library.inventory.summaryReady', { count: total })
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
  return [prefix, base, String(Date.now())].join('-')
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
  return `${value.toFixed(precision)} ${units[unitIndex] ?? 'B'}`
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

function selectDocument(documentId: string) {
  selectedDocumentId.value = documentId
}

function useDocumentTitleForSearch() {
  if (!selectedInventoryRecord.value) {
    return
  }

  const nextQuery =
    selectedInventoryRecord.value.title === t('flow.library.inventory.untitled')
      ? selectedInventoryRecord.value.subtitle
      : selectedInventoryRecord.value.title

  void router.push({
    path: '/search',
    query: { q: nextQuery },
  })
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
  () => route.query.doc,
  (value) => {
    selectedDocumentId.value = typeof value === 'string' && value.length > 0 ? value : null
  },
  { immediate: true },
)

watch(
  filteredFileInventory,
  (records) => {
    if (!records.length) {
      selectedDocumentId.value = null
      if (route.query.doc) {
        void router.replace({ query: { ...route.query, doc: undefined } })
      }
      return
    }

    const hasSelected = records.some((item) => item.id === selectedDocumentId.value)
    if (!hasSelected) {
      selectedDocumentId.value = records[0]?.id || null
    }
  },
  { immediate: true },
)

watch(
  selectedDocumentId,
  (value) => {
    const current = typeof route.query.doc === 'string' ? route.query.doc : null
    if ((value ?? null) === current) {
      return
    }

    void router.replace({
      query: {
        ...route.query,
        doc: value ?? undefined,
      },
    })
  },
)

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

  if (!selectedProjectId.value) {
    throw new Error(t('flow.library.notices.collectionBody'))
  }

  const source = await createSource({
    project_id: selectedProjectId.value,
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
    const workspaceId = getSelectedWorkspaceId() || workspaces.value[0]?.id || ''

    if (workspaceId) {
      if (workspaceId !== getSelectedWorkspaceId()) {
        setWorkspaceWithProjectReset(workspaceId)
      }
      projects.value = await fetchProjects(workspaceId)
      syncWorkspaceProjectScope(workspaces.value, projects.value)
    } else {
      projects.value = []
      setSelectedProjectId('')
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
        <RouterLink class="rr-button" to="/search" :aria-disabled="!selectedProjectId || activeJobsCount > 0">
          {{ t('flow.library.action') }}
        </RouterLink>
      </template>

      <article class="rr-panel rr-panel--accent flow-reset">
        <div class="flow-reset__hero">
          <div class="flow-reset__copy">
            <p class="rr-kicker">{{ t('flow.library.eyebrow') }}</p>
            <h2>{{ t('flow.library.title') }}</h2>
            <p>{{ t('flow.library.description') }}</p>
          </div>
          <StatusBadge :status="pageStatus.status" :label="pageStatus.label" emphasis="strong" />
        </div>

        <div class="flow-reset__scope">
          <article class="flow-reset__scope-card">
            <span>{{ t('flow.library.stats.workspace') }}</span>
            <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
          </article>
          <article class="flow-reset__scope-card">
            <span>{{ t('flow.library.stats.project') }}</span>
            <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
          </article>
          <article class="flow-reset__scope-card">
            <span>{{ t('flow.library.stats.documents') }}</span>
            <strong>{{ documents.length }}</strong>
            <small>{{ t('flow.library.stats.documentsHint') }}</small>
          </article>
        </div>

        <article
          v-if="feedback"
          class="feedback-banner"
          :data-tone="feedback.tone"
        >
          <strong>{{ feedback.title }}</strong>
          <p>{{ feedback.body }}</p>
          <p v-if="feedback.detail" class="feedback-banner__detail">{{ feedback.detail }}</p>
        </article>

        <div class="flow-reset__layout">
          <article class="rr-panel rr-stack upload-focus">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.upload.kicker') }}</p>
                <h3>{{ t('flow.library.upload.title') }}</h3>
              </div>
              <StatusBadge
                :status="selectedProjectId ? 'ready' : 'blocked'"
                :label="selectedProjectId ? t('flow.library.upload.ready') : t('flow.library.upload.needsSetup')"
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
              >

              <div class="upload-dropzone__body">
                <StatusBadge tone="info" :label="t('flow.library.upload.dropzoneIdleBadge')" />
                <h4>
                  {{ isUploadDragActive ? t('flow.library.upload.dropzoneActiveTitle') : t('flow.library.upload.dropzoneTitle') }}
                </h4>
                <p>
                  {{ isUploadDragActive ? t('flow.library.upload.dropzoneActiveBody') : t('flow.library.upload.dropzoneBody') }}
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
              >
              <p class="rr-field__hint">{{ t('flow.library.upload.titleHint') }}</p>
            </label>

            <div v-if="uploadFile && uploadSelection" class="upload-selection-card">
              <div class="upload-selection-card__meta">
                <strong>{{ uploadFile.name }}</strong>
                <span class="rr-muted">{{ uploadSelection.fileKindLabel }} · {{ formatFileSize(uploadFile.size) }}</span>
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
                {{ submitMode === 'upload' ? t('flow.library.upload.actionBusy') : t('flow.library.upload.action') }}
              </button>
              <RouterLink class="rr-button rr-button--secondary" to="/processing" v-if="!selectedProjectId">
                {{ t('flow.processing.title') }}
              </RouterLink>
            </div>
          </article>

          <article class="rr-panel rr-panel--accent rr-stack processing-overview">
            <div class="ingestion-panel__heading">
              <div class="rr-stack rr-stack--tight">
                <p class="rr-kicker">{{ t('flow.library.processing.kicker') }}</p>
                <h3>
                  {{ activeJobsCount > 0 ? t('flow.library.processing.activeTitle') : highlightedJobView ? t('flow.library.processing.latestTitle') : t('flow.library.processing.emptyTitle') }}
                </h3>
              </div>
              <StatusBadge
                v-if="highlightedJobView"
                :tone="highlightedJobView.presentation.tone"
                :label="highlightedJobView.presentation.statusLabel"
                emphasis="strong"
              />
              <StatusBadge v-else tone="info" :label="t('flow.library.processing.queueIdle')" emphasis="strong" />
            </div>

            <p class="rr-note">
              {{ highlightedJobView ? highlightedJobView.presentation.summary : t('flow.library.processing.emptyBody') }}
            </p>

            <div v-if="highlightedJobView" class="processing-human">
              <article class="processing-human__card">
                <span>{{ t('flow.library.processing.currentSource') }}</span>
                <strong>{{ highlightedJobView.sourceLabel }}</strong>
              </article>
              <article class="processing-human__card">
                <span>{{ t('flow.library.processing.currentTrigger') }}</span>
                <strong>{{ highlightedJobView.triggerLabel }}</strong>
              </article>
              <article class="processing-human__card">
                <span>{{ t('flow.library.processing.currentUpdated') }}</span>
                <strong>{{ highlightedJobView.updatedLabel ?? t('flow.library.processing.updating') }}</strong>
              </article>
              <article class="processing-human__card">
                <span>{{ t('flow.library.processing.currentDuration') }}</span>
                <strong>{{ highlightedJobView.durationLabel ?? t('flow.library.processing.notStarted') }}</strong>
              </article>
            </div>

            <div v-if="highlightedJobSteps.length" class="processing-steps processing-steps--compact">
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

            <article v-if="highlightedJobView?.error" class="processing-error">
              <strong>{{ highlightedJobView.error.title }}</strong>
              <p>{{ highlightedJobView.error.body }}</p>
              <p v-if="highlightedJobView.error.detail" class="processing-error__detail">{{ highlightedJobView.error.detail }}</p>
            </article>

            <div v-if="highlightedJobView" class="rr-action-row">
              <button type="button" class="rr-button rr-button--secondary" :disabled="queueLoading" @click="refreshProcessingState(true)">
                {{ t('flow.library.processing.refresh') }}
              </button>
              <button
                v-if="highlightedJobView.job.retryable"
                type="button"
                class="rr-button"
                :disabled="retryingJobId === highlightedJobView.job.id"
                @click="retryJob(highlightedJobView.job.id)"
              >
                {{ retryingJobId === highlightedJobView.job.id ? t('flow.library.processing.retryBusy') : t('flow.library.processing.retryAction') }}
              </button>
              <RouterLink class="rr-button" to="/search" :aria-disabled="activeJobsCount > 0 || !documents.length">
                {{ t('flow.library.action') }}
              </RouterLink>
            </div>
          </article>
        </div>

        <article class="rr-panel rr-stack file-library-panel" v-if="documents.length || recentJobs.length">
          <div class="ingestion-panel__heading">
            <div class="rr-stack rr-stack--tight">
              <p class="rr-kicker">{{ t('flow.library.inventory.kicker') }}</p>
              <h3>{{ t('flow.library.inventory.title') }}</h3>
            </div>
            <StatusBadge
              :status="documents.length ? 'ready' : activeJobsCount > 0 ? 'partial' : 'draft'"
              :label="documents.length ? inventorySummary : t('flow.library.inventory.emptyBadge')"
            />
          </div>

          <p class="rr-note">{{ t('flow.library.inventory.helper') }}</p>

          <div v-if="documents.length" class="file-library-toolbar">
            <label class="rr-field file-library-toolbar__search">
              <span class="rr-field__label">{{ t('flow.library.inventory.searchLabel') }}</span>
              <input v-model="librarySearch" class="rr-control" type="search" :placeholder="t('flow.library.inventory.searchPlaceholder')">
            </label>

            <div class="file-library-toolbar__filters">
              <button
                v-for="option in inventoryFilterOptions"
                :key="option.value"
                type="button"
                class="rr-button rr-button--secondary"
                :data-active="libraryFilter === option.value"
                @click="libraryFilter = option.value"
              >
                {{ option.label }}
              </button>
            </div>
          </div>

          <EmptyStateCard v-if="!documents.length" :title="t('flow.library.inventory.emptyTitle')" :message="t('flow.library.inventory.emptyBody')" />

          <div v-else class="file-library-grid">
            <div class="file-library-list">
              <button
                v-for="record in filteredFileInventory"
                :key="record.id"
                type="button"
                class="file-library-row"
                :data-active="selectedInventoryRecord?.id === record.id"
                @click="selectDocument(record.id)"
              >
                <div class="file-library-row__copy">
                  <div class="file-library-row__title-line">
                    <strong>{{ record.title }}</strong>
                    <StatusBadge :tone="record.statusTone" :label="record.statusLabel" />
                  </div>
                  <p class="rr-muted">{{ record.subtitle }}</p>
                  <p class="file-library-row__summary">{{ record.summaryLabel }}</p>
                </div>
                <div class="file-library-row__meta">
                  <span>{{ record.sourceKindLabel }}</span>
                  <span>{{ record.mimeLabel }}</span>
                </div>
              </button>

              <EmptyStateCard v-if="!filteredFileInventory.length" :title="t('flow.library.inventory.filteredEmptyTitle')" :message="t('flow.library.inventory.filteredEmptyBody')" />
            </div>

            <article v-if="selectedInventoryRecord" class="file-library-detail">
              <div class="file-library-detail__header">
                <div class="rr-stack rr-stack--tight">
                  <p class="rr-kicker">{{ t('flow.library.inventory.detailKicker') }}</p>
                  <h4>{{ selectedInventoryRecord.title }}</h4>
                </div>
                <StatusBadge :tone="selectedInventoryRecord.statusTone" :label="selectedInventoryRecord.statusLabel" emphasis="strong" />
              </div>

              <dl class="file-library-detail__facts">
                <div>
                  <dt>{{ t('flow.library.inventory.fields.externalKey') }}</dt>
                  <dd>{{ selectedInventoryRecord.subtitle }}</dd>
                </div>
                <div>
                  <dt>{{ t('flow.library.inventory.fields.source') }}</dt>
                  <dd>{{ selectedInventoryRecord.sourceLabel }}</dd>
                </div>
                <div>
                  <dt>{{ t('flow.library.inventory.fields.kind') }}</dt>
                  <dd>{{ selectedInventoryRecord.sourceKindLabel }}</dd>
                </div>
                <div>
                  <dt>{{ t('flow.library.inventory.fields.mime') }}</dt>
                  <dd>{{ selectedInventoryRecord.mimeLabel }}</dd>
                </div>
                <div v-if="selectedInventoryRecord.updatedAt">
                  <dt>{{ t('flow.library.inventory.fields.updated') }}</dt>
                  <dd>{{ selectedInventoryRecord.updatedAt }}</dd>
                </div>
                <div v-if="selectedInventoryRecord.checksumShort">
                  <dt>{{ t('flow.library.inventory.fields.checksum') }}</dt>
                  <dd>{{ selectedInventoryRecord.checksumShort }}</dd>
                </div>
              </dl>

              <p class="rr-note">{{ selectedInventoryRecord.summaryLabel }}</p>

              <article v-if="selectedDocumentRelatedJob" class="file-library-detail__processing">
                <div class="file-library-detail__processing-head">
                  <strong>{{ t('flow.library.inventory.processingTitle') }}</strong>
                  <StatusBadge :tone="selectedDocumentProcessingPresentation?.tone" :label="selectedDocumentProcessingPresentation?.statusLabel" />
                </div>
                <p>{{ selectedDocumentProcessingPresentation?.summary }}</p>
              </article>

              <div class="rr-action-row">
                <button type="button" class="rr-button" @click="useDocumentTitleForSearch">
                  {{ t('flow.library.inventory.searchAction') }}
                </button>
                <button type="button" class="rr-button rr-button--secondary" :disabled="queueLoading" @click="refreshProcessingState(true)">
                  {{ t('flow.library.processing.refresh') }}
                </button>
              </div>
            </article>
          </div>
        </article>
      </article>

      <CrossSurfaceGuide active-section="files" />
      <ProductSpine active-section="files" />
    </PageSection>
  </section>
</template>

<style scoped>
.ingestion-page {
  gap: 1.5rem;
}

.flow-reset,
.flow-reset__scope-card,
.flow-reset__scope,
.flow-reset__layout,
.processing-human,
.processing-human__card {
  display: grid;
  gap: 1rem;
}

.flow-reset__hero,
.ingestion-panel__heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
}

.flow-reset__hero h2,
.flow-reset__scope-card strong,
.ingestion-panel__heading h3,
.file-library-detail__header h4 {
  margin: 0;
  color: var(--rr-ink-strong);
}

.flow-reset__hero p,
.flow-reset__scope-card span,
.flow-reset__scope-card small,
.ingestion-panel__heading p {
  margin: 0;
  color: var(--rr-ink-muted);
}

.flow-reset__scope {
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
}

.flow-reset__scope-card,
.processing-human__card {
  padding: 1rem;
  border-radius: 1rem;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.08);
}

.flow-reset__layout {
  grid-template-columns: minmax(0, 1.2fr) minmax(0, 0.8fr);
  align-items: start;
}

.processing-human {
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}

.processing-steps--compact {
  gap: 0.75rem;
}

.upload-focus {
  position: sticky;
  top: 1rem;
}

.file-library-panel {
  margin-top: 1rem;
}

.file-library-detail__processing,
.file-library-detail,
.file-library-row,
.file-library-grid,
.file-library-list,
.file-library-toolbar,
.file-library-row__title-line,
.file-library-detail__header,
.file-library-detail__processing-head,
.file-library-row__meta {
  display: grid;
  gap: 0.75rem;
}

.file-library-toolbar__filters {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.file-library-row {
  width: 100%;
  text-align: left;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 1rem;
  padding: 1rem;
  background: rgba(255, 255, 255, 0.02);
}

.file-library-row[data-active='true'] {
  border-color: rgba(114, 137, 255, 0.6);
  background: rgba(114, 137, 255, 0.08);
}

.file-library-grid {
  grid-template-columns: minmax(0, 0.95fr) minmax(0, 1.05fr);
}

.file-library-detail__facts {
  display: grid;
  gap: 0.75rem;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}

.file-library-detail__facts dt,
.file-library-detail__facts dd {
  margin: 0;
}

@media (max-width: 960px) {
  .flow-reset__layout,
  .file-library-grid {
    grid-template-columns: 1fr;
  }

  .upload-focus {
    position: static;
  }
}

@media (max-width: 720px) {
  .flow-reset__hero,
  .ingestion-panel__heading {
    flex-direction: column;
  }
}
</style>
