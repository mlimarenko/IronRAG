<script setup lang="ts">
import { computed } from 'vue'
import type {
  DocumentActivityStatus,
  DocumentDetail,
  DocumentStatus,
} from 'src/models/ui/documents'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import DocumentSummaryCard from './DocumentSummaryCard.vue'

const props = defineProps<{
  open: boolean
  detail: DocumentDetail | null
  workspaceName: string | null
  loading: boolean
  error: string | null
}>()

const emit = defineEmits<{
  close: []
  append: [id: string]
  replace: [id: string]
  retry: [id: string]
  remove: [id: string]
  reprocess: [id: string]
  openInGraph: [graphNodeId: string]
  downloadText: [id: string]
}>()

const i18n = useI18n()

function formatDate(value: string | null): string {
  if (!value) {
    return '—'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString()
}

function formatDuration(value: number | null): string {
  if (value === null || value < 0) {
    return '—'
  }
  if (value < 1000) {
    return `${String(value)} ms`
  }
  if (value < 60_000) {
    return `${(value / 1000).toFixed(1)} s`
  }
  const minutes = Math.floor(value / 60_000)
  const seconds = Math.round((value % 60_000) / 1000)
  return `${String(minutes)}m ${String(seconds)}s`
}

function formatMoney(value: number | null, currency: string | null): string {
  if (value === null) {
    return '—'
  }
  const normalizedCurrency = currency ?? 'USD'
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: normalizedCurrency,
      maximumFractionDigits: 6,
    }).format(value)
  } catch {
    return `${value.toFixed(6)} ${normalizedCurrency}`
  }
}

function formatCount(value: number): string {
  return new Intl.NumberFormat().format(value)
}

function stageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  return i18n.te(key) ? i18n.t(key) : stage
}

function statusLabel(status: string): string {
  const key = `documents.status.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function activityLabel(activityStatus: DocumentActivityStatus): string {
  const key = `documents.activity.${activityStatus}`
  return i18n.te(key) ? i18n.t(key) : activityStatus
}

function activityTone(activityStatus: DocumentActivityStatus): DocumentActivityStatus {
  return activityStatus
}

function accountingLabel(status: string): string {
  const key = `documents.accounting.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function normalizationLabel(status: string): string {
  const key = `documents.normalization.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function ocrSourceLabel(value: string | null): string | null {
  if (!value) {
    return null
  }
  const key = `documents.ocrSource.${value}`
  return i18n.te(key) ? i18n.t(key) : value
}

function attributionSourceLabel(value: 'stage_native' | 'reconciled' | null): string | null {
  if (!value) {
    return null
  }
  const key = `documents.attribution.${value}`
  return i18n.te(key) ? i18n.t(key) : value
}

function revisionKindLabel(kind: string): string {
  const key = `documents.revision.kind.${kind}`
  return i18n.te(key) ? i18n.t(key) : kind
}

function attemptKindLabel(kind: string | null): string {
  if (!kind) {
    return '—'
  }
  const key = `documents.attemptKind.${kind}`
  return i18n.te(key) ? i18n.t(key) : kind
}

function mutationLabel(status: string | null): string | null {
  if (!status) {
    return null
  }
  const key = `documents.mutation.status.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function mutationKindLabel(kind: string | null): string | null {
  if (!kind) {
    return null
  }
  const key = `documents.mutation.kind.${kind}`
  return i18n.te(key) ? i18n.t(key) : kind
}

function accountingTone(status: string): DocumentStatus {
  switch (status) {
    case 'priced':
      return 'ready'
    case 'in_flight_unsettled':
      return 'processing'
    case 'partial':
      return 'ready_no_graph'
    default:
      return 'failed'
  }
}

function mutationTone(status: string | null): DocumentStatus {
  switch (status) {
    case 'accepted':
    case 'reconciling':
      return 'processing'
    case 'failed':
      return 'failed'
    default:
      return 'ready'
  }
}

function benchmarkTone(status: string): DocumentStatus {
  switch (status) {
    case 'completed':
    case 'skipped':
      return 'ready'
    case 'failed':
      return 'failed'
    case 'started':
      return 'processing'
    default:
      return 'queued'
  }
}

function activityNarrative(
  activityStatus: DocumentActivityStatus,
  stalledReason: string | null,
  lastActivityAt: string | null,
): string | null {
  if (stalledReason) {
    return stalledReason
  }
  if (lastActivityAt) {
    return i18n.t('documents.lastActivityAt', { value: formatDate(lastActivityAt) })
  }
  if (activityStatus === 'blocked' || activityStatus === 'retrying' || activityStatus === 'stalled') {
    return i18n.t(`documents.activityDescriptions.${activityStatus}`)
  }
  return null
}

const localizedSummary = computed(() => {
  const detail = props.detail
  if (!detail) {
    return ''
  }

  const chunkCount = detail.extractedStats.chunkCount ?? 0
  if (detail.status === 'ready') {
    return i18n.t('documents.details.summary.ready', { count: chunkCount })
  }
  if (detail.status === 'ready_no_graph') {
    return i18n.t('documents.details.summary.readyNoGraph', { count: chunkCount })
  }
  if (detail.status === 'failed') {
    return i18n.t('documents.details.summary.failed')
  }
  if (detail.status === 'processing') {
    return i18n.t('documents.details.summary.processing')
  }
  return i18n.t('documents.details.summary.queued')
})

const headlineCards = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  const cards = [
    {
      key: 'cost',
      tone: accountingTone(detail.accountingStatus),
      value: formatMoney(detail.totalEstimatedCost, detail.currency),
      label: i18n.t('documents.details.totalCost'),
    },
    {
      key: 'accounting',
      tone: accountingTone(detail.accountingStatus),
      value: accountingLabel(detail.accountingStatus),
      label: i18n.t('documents.details.accountingStatus'),
    },
    {
      key: 'attempt',
      tone: detail.status,
      value: `#${String(detail.latestAttemptNo)}`,
      label: i18n.t('documents.details.latestAttempt'),
    },
  ] satisfies { key: string; tone: DocumentStatus; value: string; label: string }[]

  if (detail.settledEstimatedCost !== null) {
    cards.splice(1, 0, {
      key: 'settled-cost',
      tone: 'ready',
      value: formatMoney(detail.settledEstimatedCost, detail.currency),
      label: i18n.t('documents.details.settledCost'),
    })
  }

  if (detail.inFlightEstimatedCost !== null || detail.inFlightStageCount > 0) {
    cards.splice(2, 0, {
      key: 'in-flight-cost',
      tone: 'processing',
      value: formatMoney(detail.inFlightEstimatedCost, detail.currency),
      label: i18n.t('documents.details.inFlightCost'),
    })
  }

  if (eyebrowStageLabel.value) {
    cards.splice(2, 0, {
      key: 'stage',
      tone: detail.status,
      value: eyebrowStageLabel.value,
      label: i18n.t('documents.headers.stage'),
    })
  }

  return cards
})

const statsCards = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    {
      key: 'chunks',
      label: i18n.t('documents.details.chunkCount'),
      value: detail.extractedStats.chunkCount ?? '—',
    },
    {
      key: 'nodes',
      label: i18n.t('documents.details.graphNodes'),
      value: detail.graphStats.nodeCount,
    },
    {
      key: 'evidence',
      label: i18n.t('documents.details.graphEvidence'),
      value: detail.graphStats.evidenceCount,
    },
    {
      key: 'pages',
      label: i18n.t('documents.details.pageCount'),
      value: detail.extractedStats.pageCount ?? '—',
    },
  ]
})

const metadataRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }

  return [
    { key: 'type', label: i18n.t('documents.headers.type'), value: detail.fileType },
    { key: 'size', label: i18n.t('documents.headers.size'), value: detail.fileSizeLabel },
    { key: 'uploaded', label: i18n.t('documents.headers.uploaded'), value: formatDate(detail.uploadedAt) },
    { key: 'lastActivity', label: i18n.t('documents.details.lastActivity'), value: formatDate(detail.lastActivityAt) },
    { key: 'extractor', label: i18n.t('documents.details.extractionKind'), value: detail.extractedStats.extractionKind ?? '—' },
    { key: 'revisionStatus', label: i18n.t('documents.details.activeRevisionStatus'), value: detail.activeRevisionStatus ?? '—' },
  ]
})

const revisionTimeline = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return detail.revisionHistory.map((revision) => ({
    key: revision.id,
    title: `#${String(revision.revisionNo)} · ${revisionKindLabel(revision.revisionKind)}`,
    subtitle: [
      revision.sourceFileName,
      revision.status,
      revision.isActive ? i18n.t('documents.details.currentRevision') : null,
    ]
      .filter(Boolean)
      .join(' · '),
    body: revision.appendedTextExcerpt ?? null,
    timestamp: [
      formatDate(revision.acceptedAt),
      revision.activatedAt ? formatDate(revision.activatedAt) : null,
    ]
      .filter(Boolean)
      .join(' · '),
    isCurrent: revision.isActive,
    isDone: revision.status === 'active' || revision.status === 'superseded',
  }))
})

const attemptSections = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return detail.attempts.map((attempt) => ({
    key: `${detail.id}-${String(attempt.attemptNo)}`,
    heading: i18n.t('documents.details.attemptLabel', { number: attempt.attemptNo }),
    subtitle: [
      attemptKindLabel(attempt.attemptKind),
      attempt.revisionNo ? `#${String(attempt.revisionNo)}` : null,
      attempt.status,
    ]
      .filter(Boolean)
      .join(' · '),
    summaryLines: [
      `${i18n.t('documents.details.activityStatus')}: ${activityLabel(attempt.activityStatus)}`,
      `${i18n.t('documents.details.lastActivity')}: ${formatDate(attempt.lastActivityAt)}`,
      `${i18n.t('documents.details.queueTime')}: ${formatDuration(attempt.queueElapsedMs)}`,
      `${i18n.t('documents.details.totalTime')}: ${formatDuration(attempt.totalElapsedMs)}`,
      `${i18n.t('documents.details.accountingStatus')}: ${accountingLabel(
        attempt.summary.accountingStatus,
      )}`,
      `${i18n.t('documents.details.totalCost')}: ${formatMoney(
        attempt.summary.totalEstimatedCost,
        attempt.summary.currency,
      )}`,
      ...(attempt.summary.settledEstimatedCost !== null
        ? [
            `${i18n.t('documents.details.settledCost')}: ${formatMoney(
              attempt.summary.settledEstimatedCost,
              attempt.summary.currency,
            )}`,
          ]
        : []),
      ...(attempt.summary.inFlightEstimatedCost !== null || attempt.summary.inFlightStageCount > 0
        ? [
            `${i18n.t('documents.details.inFlightCost')}: ${formatMoney(
              attempt.summary.inFlightEstimatedCost,
              attempt.summary.currency,
            )}`,
            `${i18n.t('documents.details.inFlightStages')}: ${formatCount(
              attempt.summary.inFlightStageCount,
            )}`,
          ]
        : []),
      ...(attempt.summary.missingStageCount > 0
        ? [
            `${i18n.t('documents.details.missingAccountingStages')}: ${formatCount(
              attempt.summary.missingStageCount,
            )}`,
          ]
        : []),
    ],
    partialHistoryReason: attempt.partialHistory ? attempt.partialHistoryReason : null,
    benchmarks: attempt.benchmarks.map((benchmark, index) => ({
      key: `${String(attempt.attemptNo)}-${benchmark.stage}-${benchmark.startedAt}-${String(index)}`,
      tone: benchmarkTone(benchmark.status),
      title: stageLabel(benchmark.stage),
      subtitle: [
        formatDate(benchmark.startedAt),
        benchmark.finishedAt ? formatDate(benchmark.finishedAt) : null,
        formatDuration(benchmark.elapsedMs),
      ]
        .filter(Boolean)
        .join(' · '),
      provider: [benchmark.providerKind, benchmark.modelName].filter(Boolean).join(' / ') || null,
      message: benchmark.message,
      accounting: benchmark.accounting
        ? [
            accountingLabel(benchmark.accounting.pricingStatus),
            attributionSourceLabel(benchmark.accounting.attributionSource),
            benchmark.accounting.inFlightEstimatedCost !== null
              ? `${i18n.t('documents.details.inFlightCost')}: ${formatMoney(
                  benchmark.accounting.inFlightEstimatedCost,
                  benchmark.accounting.currency,
                )}`
              : benchmark.accounting.settledEstimatedCost !== null
                ? `${i18n.t('documents.details.settledCost')}: ${formatMoney(
                    benchmark.accounting.settledEstimatedCost,
                    benchmark.accounting.currency,
                  )}`
                : formatMoney(benchmark.accounting.estimatedCost, benchmark.accounting.currency),
          ]
            .filter(Boolean)
            .join(' · ')
        : null,
    })),
  }))
})

const showOpenInGraph = computed(() => Boolean(props.detail?.graphNodeId))
const showMutationWarning = computed(() => Boolean(props.detail?.mutation.warning))
const showMutationBadge = computed(() => {
  const mutation = props.detail?.mutation
  if (!mutation?.status) {
    return false
  }

  if (mutation.status === 'failed') {
    return true
  }

  return mutation.kind === 'delete'
})
const showActivityWarning = computed(() => {
  const activityStatus = props.detail?.activityStatus
  return (
    activityStatus === 'blocked' ||
    activityStatus === 'retrying' ||
    activityStatus === 'stalled'
  )
})
const deleteInProgress = computed(() => {
  const mutation = props.detail?.mutation
  return (
    mutation?.kind === 'delete' &&
    (mutation.status === 'accepted' || mutation.status === 'reconciling')
  )
})
const mutationLocked = computed(
  () =>
    props.detail?.mutation.status === 'accepted' ||
    props.detail?.mutation.status === 'reconciling',
)
const activityLocked = computed(
  () =>
    props.detail?.activityStatus === 'blocked' ||
    props.detail?.activityStatus === 'retrying' ||
    props.detail?.activityStatus === 'stalled',
)
const disableReprocess = computed(
  () =>
    props.detail?.status === 'processing' ||
    props.detail?.status === 'queued' ||
    mutationLocked.value ||
    props.detail?.activityStatus === 'retrying',
)
const showActivityBadge = computed(() => {
  const detail = props.detail
  if (!detail) {
    return false
  }

  return ['blocked', 'retrying', 'stalled'].includes(detail.activityStatus)
})

const heroMetaLine = computed(() => {
  const detail = props.detail
  if (!detail) {
    return ''
  }

  return [
    detail.fileType,
    detail.fileSizeLabel,
    formatDate(detail.uploadedAt),
  ]
    .filter(Boolean)
    .join(' · ')
})

const eyebrowStageLabel = computed(() => {
  const detail = props.detail
  if (!detail) {
    return null
  }

  const label = stageLabel(detail.stage)
  const normalized = label.toLowerCase()
  if (normalized === statusLabel(detail.status).toLowerCase()) {
    return null
  }
  if (normalized === 'принято' || normalized === 'accepted') {
    return null
  }
  return label
})

const showRevisionHistory = computed(() => revisionTimeline.value.length > 0)
const showAttemptSections = computed(() => attemptSections.value.length > 0)
const showExtractedPreview = computed(() => Boolean(props.detail?.extractedStats.previewText))
const showGraphProgressCard = computed(() => {
  const detail = props.detail
  return (
    Boolean(detail) &&
    detail?.status === 'processing' &&
    detail.stage === 'extracting_graph' &&
    detail.progressPercent !== null
  )
})
const graphProgressSummary = computed(() => {
  const detail = props.detail
  const progressPercent = detail?.progressPercent
  if (!detail || progressPercent === null || progressPercent === undefined) {
    return null
  }

  return i18n.t('documents.details.graphProgress.summary', {
    progress: progressPercent,
    chunks: detail.extractedStats.chunkCount ?? 0,
  })
})
const graphProgressMeta = computed(() => {
  const detail = props.detail
  const progressPercent = detail?.progressPercent
  if (!detail || progressPercent === null || progressPercent === undefined) {
    return []
  }

  return [
    `${i18n.t('documents.details.graphProgress.stage')}: ${stageLabel(detail.stage)}`,
    `${i18n.t('documents.details.graphProgress.percent')}: ${String(progressPercent)}%`,
    detail.lastActivityAt
      ? `${i18n.t('documents.details.lastActivity')}: ${formatDate(detail.lastActivityAt)}`
      : null,
  ].filter((value): value is string => Boolean(value))
})
const extractedMetaLines = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }

  return [
    `${i18n.t('documents.details.normalizationStatus')}: ${normalizationLabel(
      detail.extractedStats.normalizationStatus,
    )}`,
    `${i18n.t('documents.details.warningCount')}: ${formatCount(detail.extractedStats.warningCount)}`,
    detail.extractedStats.ocrSource
      ? `${i18n.t('documents.details.ocrSource')}: ${ocrSourceLabel(detail.extractedStats.ocrSource) ?? detail.extractedStats.ocrSource}`
      : null,
  ].filter((value): value is string => Boolean(value))
})
</script>

<template>
  <div
    v-if="props.open"
    class="rr-documents-drawer-shell"
    @click.self="emit('close')"
  >
    <aside class="rr-documents-drawer">
      <div class="rr-documents-drawer__main">
        <header class="rr-documents-drawer__header">
          <div class="rr-documents-drawer__eyebrow">
            <StatusPill
              v-if="props.detail"
              :tone="props.detail.status"
              :label="statusLabel(props.detail.status)"
            />
            <StatusPill
              v-if="props.detail && showActivityBadge"
              :tone="activityTone(props.detail.activityStatus)"
              :label="activityLabel(props.detail.activityStatus)"
            />
            <StatusPill
              v-if="props.detail && showMutationBadge && mutationLabel(props.detail.mutation.status)"
              :tone="mutationTone(props.detail.mutation.status)"
              :label="mutationLabel(props.detail.mutation.status)!"
            />
            <span
              v-if="eyebrowStageLabel"
              class="rr-documents-drawer__eyebrow-copy"
            >
              {{ eyebrowStageLabel }}
            </span>
          </div>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click="emit('close')"
          >
            {{ $t('documents.details.close') }}
          </button>
        </header>

        <div
          v-if="props.loading"
          class="rr-documents-drawer__loading"
        >
          {{ $t('documents.loadingDetail') }}
        </div>
        <p
          v-else-if="props.error"
          class="rr-error-card"
        >
          {{ props.error }}
        </p>

        <template v-else-if="props.detail">
          <section class="rr-documents-drawer__hero">
            <div>
              <h2 :title="props.detail.fileName">{{ props.detail.fileName }}</h2>
              <span
                class="rr-documents-drawer__hero-meta"
                :title="heroMetaLine"
              >
                {{ heroMetaLine }}
              </span>
              <p>{{ localizedSummary }}</p>
            </div>
          </section>

          <section class="rr-documents-drawer__stats-grid">
            <DocumentSummaryCard
              v-for="card in headlineCards"
              :key="card.key"
              :tone="card.tone"
              :value="card.value"
              :label="card.label"
            />
          </section>

          <section class="rr-documents-drawer__stats-grid">
            <article
              v-for="card in statsCards"
              :key="card.key"
              class="rr-documents-drawer__stat-card"
            >
              <span>{{ card.label }}</span>
              <strong>{{ card.value }}</strong>
            </article>
          </section>

          <section class="rr-documents-drawer__meta-grid">
            <article
              v-for="item in metadataRows"
              :key="item.key"
              class="rr-documents-drawer__meta-item"
              :title="item.value"
            >
              <span>{{ item.label }}</span>
              <strong>{{ item.value }}</strong>
            </article>
          </section>

          <section
            v-if="showActivityWarning"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.activityWarning') }}</h4>
            <p>
              {{
                activityNarrative(
                  props.detail.activityStatus,
                  props.detail.stalledReason,
                  props.detail.lastActivityAt,
                ) || $t(`documents.activityDescriptions.${props.detail.activityStatus}`)
              }}
            </p>
          </section>

          <section
            v-if="props.detail.errorMessage"
            class="rr-documents-drawer__soft-card rr-documents-drawer__soft-card--danger"
          >
            <h4>{{ $t('documents.details.error') }}</h4>
            <p>{{ props.detail.errorMessage }}</p>
          </section>

          <section
            v-if="props.detail.mutation.status"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.activeMutation') }}</h4>
            <p>
              {{
                [
                  mutationKindLabel(props.detail.mutation.kind),
                  mutationLabel(props.detail.mutation.status),
                  props.detail.activeRevisionNo ? `#${String(props.detail.activeRevisionNo)}` : null,
                ]
                  .filter(Boolean)
                  .join(' · ')
              }}
            </p>
          </section>

          <section
            v-if="props.detail.partialHistory"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.partialHistory') }}</h4>
            <p>
              {{ props.detail.partialHistoryReason || $t('documents.details.partialHistoryFallback') }}
            </p>
          </section>

          <section
            v-if="showMutationWarning"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.mutationWarning') }}</h4>
            <p>{{ props.detail.mutation.warning }}</p>
          </section>

          <section
            v-if="showGraphProgressCard"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.graphProgress.title') }}</h4>
            <p>{{ graphProgressSummary }}</p>
            <p class="rr-documents-drawer__microcopy">
              {{ graphProgressMeta.join(' · ') }}
            </p>
          </section>

          <section
            v-if="showExtractedPreview"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.preview') }}</h4>
            <p class="rr-documents-drawer__microcopy">
              {{ extractedMetaLines.join(' · ') }}
            </p>
            <pre class="rr-documents-drawer__preview">{{ props.detail.extractedStats.previewText }}</pre>
            <p
              v-if="props.detail.extractedStats.previewTruncated"
              class="rr-documents-drawer__microcopy"
            >
              {{ $t('documents.details.previewTruncated') }}
            </p>
          </section>

          <section
            v-if="props.detail.extractedStats.warnings.length"
            class="rr-documents-drawer__soft-card"
          >
            <h4>{{ $t('documents.details.warnings') }}</h4>
            <ul class="rr-documents-drawer__list">
              <li
                v-for="warning in props.detail.extractedStats.warnings"
                :key="warning"
              >
                {{ warning }}
              </li>
            </ul>
          </section>

          <section class="rr-documents-drawer__soft-card">
            <h4>{{ $t('documents.details.graphStats') }}</h4>
            <p>
              {{ $t('documents.details.graphContributionSummary', {
                nodes: props.detail.graphStats.nodeCount,
                edges: props.detail.graphStats.edgeCount,
                evidence: props.detail.graphStats.evidenceCount,
              }) }}
            </p>
          </section>

          <details
            v-if="showRevisionHistory"
            class="rr-documents-drawer__soft-card rr-documents-drawer__accordion"
          >
            <summary>
              <span>{{ $t('documents.details.revisionHistory') }}</span>
              <span>{{ revisionTimeline.length }}</span>
            </summary>
            <ol class="rr-documents-drawer__timeline">
              <li
                v-for="revision in revisionTimeline"
                :key="revision.key"
                class="rr-documents-drawer__timeline-item"
                :class="{ 'is-current': revision.isCurrent, 'is-done': revision.isDone }"
              >
                <div class="rr-documents-drawer__timeline-marker" />
                <div class="rr-documents-drawer__timeline-body">
                  <strong>{{ revision.title }}</strong>
                  <span>{{ revision.subtitle }}</span>
                  <span>{{ revision.timestamp }}</span>
                  <p v-if="revision.body">{{ revision.body }}</p>
                </div>
              </li>
            </ol>
          </details>

          <template v-if="showAttemptSections">
            <details
              v-for="attempt in attemptSections"
              :key="attempt.key"
              class="rr-documents-drawer__soft-card rr-documents-drawer__accordion"
            >
              <summary>
                <span>{{ attempt.heading }}</span>
                <span>{{ attempt.subtitle }}</span>
              </summary>
              <ul class="rr-documents-drawer__list">
                <li
                  v-for="line in attempt.summaryLines"
                  :key="line"
                >
                  {{ line }}
                </li>
              </ul>
              <p v-if="attempt.partialHistoryReason">{{ attempt.partialHistoryReason }}</p>
              <ol class="rr-documents-drawer__timeline">
                <li
                  v-for="benchmark in attempt.benchmarks"
                  :key="benchmark.key"
                  class="rr-documents-drawer__timeline-item"
                  :class="{ 'is-current': benchmark.tone === 'processing', 'is-done': benchmark.tone === 'ready' }"
                >
                  <div class="rr-documents-drawer__timeline-marker" />
                  <div class="rr-documents-drawer__timeline-body">
                    <strong>{{ benchmark.title }}</strong>
                    <span>{{ benchmark.subtitle }}</span>
                    <p v-if="benchmark.provider">{{ benchmark.provider }}</p>
                    <p v-if="benchmark.accounting">{{ benchmark.accounting }}</p>
                    <p v-if="benchmark.message">{{ benchmark.message }}</p>
                  </div>
                </li>
              </ol>
            </details>
          </template>

          <details
            v-else
            class="rr-documents-drawer__soft-card rr-documents-drawer__accordion"
          >
            <summary>
              <span>{{ $t('documents.details.history') }}</span>
              <span>{{ props.detail.processingHistory.length }}</span>
            </summary>
            <ol class="rr-documents-drawer__timeline">
              <li
                v-for="item in props.detail.processingHistory"
                :key="`${String(item.attemptNo)}-${item.stage}-${item.startedAt}`"
                class="rr-documents-drawer__timeline-item"
                :class="{ 'is-current': props.detail.status === 'processing', 'is-done': item.status === 'completed' }"
              >
                <div class="rr-documents-drawer__timeline-marker" />
                <div class="rr-documents-drawer__timeline-body">
                  <strong>{{ stageLabel(item.stage) }}</strong>
                  <span>{{ formatDate(item.startedAt) }} · {{ formatDate(item.finishedAt) }}</span>
                  <p v-if="item.errorMessage">{{ item.errorMessage }}</p>
                </div>
              </li>
            </ol>
          </details>
        </template>
      </div>

      <aside
        v-if="props.detail"
        class="rr-documents-drawer__rail"
      >
        <h4>{{ $t('documents.details.actionsTitle') }}</h4>
        <p>{{ $t('documents.details.actionsSubtitle') }}</p>

        <button
          v-if="showOpenInGraph"
          class="rr-button"
          type="button"
          @click="emit('openInGraph', props.detail.graphNodeId!)"
        >
          {{ $t('documents.details.openInGraph') }}
        </button>

        <button
          v-if="props.detail.canAppend"
          class="rr-button rr-button--ghost"
          type="button"
          :disabled="mutationLocked || activityLocked"
          @click="emit('append', props.detail.id)"
        >
          {{ $t('documents.actions.append') }}
        </button>

        <button
          v-if="props.detail.canReplace"
          class="rr-button rr-button--ghost"
          type="button"
          :disabled="mutationLocked || activityLocked"
          @click="emit('replace', props.detail.id)"
        >
          {{ $t('documents.actions.replace') }}
        </button>

        <button
          class="rr-button rr-button--ghost"
          type="button"
          :disabled="disableReprocess"
          @click="emit('reprocess', props.detail.id)"
        >
          {{ $t('documents.details.reprocess') }}
        </button>

        <button
          class="rr-button rr-button--ghost"
          type="button"
          :disabled="!props.detail.canDownloadText"
          @click="emit('downloadText', props.detail.id)"
        >
          {{ $t('documents.details.downloadText') }}
        </button>

        <button
          v-if="props.detail.status === 'failed'"
          class="rr-button rr-button--ghost"
          type="button"
          @click="emit('retry', props.detail.id)"
        >
          {{ $t('documents.actions.retry') }}
        </button>

        <button
          v-if="props.detail.canRemove || deleteInProgress"
          class="rr-button rr-button--ghost is-danger"
          type="button"
          :disabled="!props.detail.canRemove || mutationLocked || activityLocked"
          @click="emit('remove', props.detail.id)"
        >
          {{ deleteInProgress ? $t('documents.actions.removing') : $t('documents.actions.remove') }}
        </button>

        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.fileSize') }}</span>
          <strong :title="props.detail.fileSizeLabel">{{ props.detail.fileSizeLabel }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.pageCount') }}</span>
          <strong>{{ props.detail.extractedStats.pageCount ?? '—' }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.accountingStatus') }}</span>
          <strong :title="accountingLabel(props.detail.accountingStatus)">{{ accountingLabel(props.detail.accountingStatus) }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.inFlightStages') }}</span>
          <strong>{{ formatCount(props.detail.inFlightStageCount) }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.missingAccountingStages') }}</span>
          <strong>{{ formatCount(props.detail.missingStageCount) }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.checksum') }}</span>
          <strong :title="props.detail.extractedStats.checksum ?? '—'">{{ props.detail.extractedStats.checksum ?? '—' }}</strong>
        </div>
        <div class="rr-documents-drawer__rail-card">
          <span>{{ $t('documents.details.requestedBy') }}</span>
          <strong :title="props.detail.requestedBy ?? '—'">{{ props.detail.requestedBy ?? '—' }}</strong>
        </div>
      </aside>
    </aside>
  </div>
</template>
