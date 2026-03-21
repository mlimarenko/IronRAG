<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import DocumentSummaryCard from './DocumentSummaryCard.vue'
import type {
  DocumentActivityStatus,
  DocumentCollectionDiagnostics,
  DocumentDetail,
  DocumentKnowledgeReadiness,
  DocumentStatus,
} from 'src/models/ui/documents'

type StatusPillTone =
  | DocumentStatus
  | 'active'
  | 'blocked'
  | 'retrying'
  | 'stalled'

interface DrawerTruthState {
  tone: StatusPillTone
  label: string
  note: string
}

const props = defineProps<{
  open: boolean
  detail: DocumentDetail | null
  graphBackend: string | null
  libraryDiagnostics: DocumentCollectionDiagnostics | null
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
  return parsed.toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  })
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

function statusLabel(status: string): string {
  const key = `documents.status.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function stageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  return i18n.te(key) ? i18n.t(key) : stage
}

function activityLabel(status: DocumentActivityStatus): string {
  const key = `documents.activity.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function revisionKindLabel(kind: string | null): string {
  if (!kind) {
    return '—'
  }
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

function mutationTone(status: string | null): DocumentStatus {
  if (status === 'failed') {
    return 'failed'
  }
  if (status === 'accepted' || status === 'reconciling') {
    return 'processing'
  }
  return 'ready'
}

function truthToneReady(status: DocumentStatus): DocumentStatus {
  if (status === 'ready_no_graph') {
    return 'ready_no_graph'
  }
  if (status === 'ready') {
    return 'ready'
  }
  if (status === 'failed') {
    return 'failed'
  }
  return status === 'queued' ? 'queued' : 'processing'
}

function summaryTone(tone: StatusPillTone): DocumentStatus {
  if (tone === 'active' || tone === 'blocked' || tone === 'retrying' || tone === 'stalled') {
    return 'processing'
  }
  return tone
}

function readinessTone(status: string): DocumentStatus {
  if (status === 'ready') {
    return 'ready'
  }
  if (status === 'failed') {
    return 'failed'
  }
  if (status === 'accepted' || status === 'processing' || status === 'queued') {
    return 'processing'
  }
  return 'processing'
}

function readinessLabel(status: string): string {
  switch (status) {
    case 'ready':
      return i18n.t('documents.status.ready')
    case 'accepted':
      return i18n.t('documents.mutation.status.accepted')
    case 'processing':
      return i18n.t('documents.status.processing')
    case 'queued':
      return i18n.t('documents.status.queued')
    case 'failed':
      return i18n.t('documents.status.failed')
    default:
      return status
  }
}

function readinessCardNote(value: string | null): string {
  return value
    ? i18n.t('documents.details.readyAt', { value: formatDate(value) })
    : i18n.t('documents.details.stillSettling')
}

const resolvedDiagnostics = computed(() => {
  return props.detail?.collectionDiagnostics ?? props.libraryDiagnostics ?? null
})

const knowledgeReadiness = computed<DocumentKnowledgeReadiness | null>(
  () => props.detail?.knowledgeReadiness ?? null,
)
const backendLabel = computed(
  () => props.graphBackend ?? i18n.t('documents.workspace.backends.canonical_arango'),
)

const readinessCards = computed(() => {
  const readiness = knowledgeReadiness.value
  if (!readiness) {
    return []
  }

  return [
    {
      key: 'text',
      label: i18n.t('documents.details.textReadiness'),
      tone: readinessTone(readiness.textState),
      value: readinessLabel(readiness.textState),
      note: readinessCardNote(readiness.textReadableAt),
    },
    {
      key: 'vector',
      label: i18n.t('documents.details.vectorReadiness'),
      tone: readinessTone(readiness.vectorState),
      value: readinessLabel(readiness.vectorState),
      note: readinessCardNote(readiness.vectorReadyAt),
    },
    {
      key: 'graph',
      label: i18n.t('documents.details.graphReadiness'),
      tone: readinessTone(readiness.graphState),
      value: readinessLabel(readiness.graphState),
      note: readinessCardNote(readiness.graphReadyAt),
    },
  ]
})

const readableTruth = computed<DrawerTruthState | null>(() => {
  const detail = props.detail
  if (!detail) {
    return null
  }

  const readableNow =
    Boolean(detail.extractedStats.previewText?.trim()) ||
    detail.status === 'ready' ||
    detail.status === 'ready_no_graph' ||
    resolvedDiagnostics.value?.graphHealth?.isRuntimeReadable === true

  if (readableNow) {
    return {
      tone: truthToneReady(detail.status),
      label:
        detail.status === 'ready_no_graph'
          ? i18n.t('documents.details.truth.readableWithGraphCatchUp')
          : i18n.t('documents.details.truth.readable'),
      note:
        detail.extractedStats.previewTruncated || detail.status === 'ready_no_graph'
          ? i18n.t('documents.details.truthNotes.readableWhileCatchingUp', {
              backend: backendLabel.value,
            })
          : i18n.t('documents.details.truthNotes.readableReady'),
    }
  }

  return {
    tone: detail.status === 'failed' ? 'failed' : detail.status === 'queued' ? 'queued' : 'processing',
    label:
      detail.status === 'failed'
        ? i18n.t('documents.details.truth.readableUnavailable')
        : i18n.t('documents.details.truth.notReadableYet'),
    note: detail.errorMessage ?? stageLabel(detail.stage),
  }
})

const settledTruth = computed<DrawerTruthState | null>(() => {
  const detail = props.detail
  if (!detail) {
    return null
  }

  const terminal = resolvedDiagnostics.value?.terminalOutcome ?? null
  const settlement = resolvedDiagnostics.value?.settlement ?? null
  const pendingMutation =
    detail.mutation.status === 'accepted' || detail.mutation.status === 'reconciling'
  const activeWork =
    pendingMutation ||
    ['queued', 'active', 'blocked', 'retrying', 'stalled'].includes(detail.activityStatus) ||
    detail.inFlightStageCount > 0 ||
    detail.missingStageCount > 0

  if (terminal?.terminalState === 'failed_with_residual_work') {
    return {
      tone: 'failed' as DocumentStatus,
      label: i18n.t('documents.details.truth.residualFailure'),
      note:
        terminal.residualReason ??
        detail.errorMessage ??
        i18n.t('documents.details.truthNotes.downstreamFailure'),
    }
  }

  if (settlement?.isFullySettled || terminal?.terminalState === 'fully_settled') {
    return {
      tone: 'ready' as DocumentStatus,
      label: i18n.t('documents.details.truth.settled'),
      note: terminal?.settledAt
        ? i18n.t('documents.terminal.blockers.settledAt', {
            value: formatDate(terminal.settledAt),
          })
        : settlement?.settledAt
          ? i18n.t('documents.terminal.blockers.settledAt', {
              value: formatDate(settlement.settledAt),
            })
          : i18n.t('documents.details.truthNotes.noInflightWork'),
    }
  }

  if (activeWork) {
    return {
      tone: 'processing' as DocumentStatus,
      label: i18n.t('documents.details.truth.settling'),
      note: [
        detail.inFlightStageCount > 0
          ? i18n.t('documents.details.liveStages', { count: detail.inFlightStageCount })
          : null,
        detail.missingStageCount > 0
          ? i18n.t('documents.details.missingStages', { count: detail.missingStageCount })
          : null,
      ]
        .filter(Boolean)
        .join(' · ') || i18n.t('documents.details.truthNotes.settlingInProgress'),
    }
  }

  return {
    tone: detail.status === 'failed' ? ('failed' as DocumentStatus) : ('ready_no_graph' as DocumentStatus),
    label:
      detail.status === 'failed'
        ? i18n.t('documents.details.truth.failed')
        : i18n.t('documents.details.truth.open'),
    note: i18n.t('documents.details.truthNotes.readableStoredPendingSettlement', {
      backend: backendLabel.value,
    }),
  }
})

const summaryCards = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }

  return [
    {
      key: 'revision',
      tone: truthToneReady(detail.status),
      value: detail.activeRevisionNo ? `#${String(detail.activeRevisionNo)}` : '—',
      label: i18n.t('documents.details.activeRevision'),
    },
    {
      key: 'attempt',
      tone: detail.status,
      value: detail.latestAttemptNo > 0 ? `#${String(detail.latestAttemptNo)}` : '—',
      label: i18n.t('documents.details.latestAttempt'),
    },
    {
      key: 'stage',
      tone: detail.status,
      value: stageLabel(detail.stage),
      label: i18n.t('documents.details.currentStage'),
    },
    {
      key: 'cost',
      tone: summaryTone(settledTruth.value?.tone ?? detail.status),
      value:
        detail.settledEstimatedCost !== null
          ? formatMoney(detail.settledEstimatedCost, detail.currency)
          : formatMoney(detail.totalEstimatedCost, detail.currency),
      label:
        detail.settledEstimatedCost !== null
          ? i18n.t('documents.details.settledCost')
          : i18n.t('documents.details.estimatedCost'),
    },
  ]
})

const metadataRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    { key: 'workspace', label: i18n.t('documents.details.workspace'), value: props.workspaceName ?? '—' },
    { key: 'library', label: i18n.t('documents.details.library'), value: detail.libraryName },
    { key: 'uploaded', label: i18n.t('documents.details.uploaded'), value: formatDate(detail.uploadedAt) },
    { key: 'fileType', label: i18n.t('documents.details.fileType'), value: detail.fileType },
    { key: 'fileSize', label: i18n.t('documents.details.fileSize'), value: detail.fileSizeLabel },
    { key: 'requestedBy', label: i18n.t('documents.details.requestedBy'), value: detail.requestedBy ?? '—' },
    { key: 'accounting', label: i18n.t('documents.details.accounting'), value: detail.accountingStatus },
  ]
})

const mutationRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    {
      key: 'kind',
      label: i18n.t('documents.details.operation'),
      value: mutationKindLabel(detail.mutation.kind) ?? i18n.t('documents.details.noActiveMutation'),
    },
    {
      key: 'status',
      label: i18n.t('documents.headers.status'),
      value: mutationLabel(detail.mutation.status) ?? i18n.t('documents.details.idle'),
    },
    {
      key: 'warning',
      label: i18n.t('documents.details.warnings'),
      value: detail.mutation.warning ?? '—',
    },
    {
      key: 'error',
      label: i18n.t('documents.details.error'),
      value: detail.errorMessage ?? '—',
    },
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
      revision.status,
      revision.sourceFileName,
      revision.isActive ? i18n.t('documents.details.activeHead') : null,
    ]
      .filter(Boolean)
      .join(' · '),
    timestamp: [
      i18n.t('documents.details.acceptedAt', { value: formatDate(revision.acceptedAt) }),
      revision.activatedAt
        ? i18n.t('documents.details.activatedAt', { value: formatDate(revision.activatedAt) })
        : null,
      revision.supersededAt
        ? i18n.t('documents.details.supersededAt', { value: formatDate(revision.supersededAt) })
        : null,
    ]
      .filter(Boolean)
      .join(' · '),
    body: revision.appendedTextExcerpt,
  }))
})

const attemptSections = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return detail.attempts.map((attempt) => ({
    key: `${detail.id}-${String(attempt.attemptNo)}`,
    heading: i18n.t('documents.details.attemptHeading', { number: attempt.attemptNo }),
    subtitle: [
      attemptKindLabel(attempt.attemptKind),
      attempt.revisionNo
        ? i18n.t('documents.details.revisionLabel', { number: attempt.revisionNo })
        : null,
      attempt.status,
      activityLabel(attempt.activityStatus),
    ]
      .filter(Boolean)
      .join(' · '),
    meta: [
      i18n.t('documents.details.lastActivityAt', { value: formatDate(attempt.lastActivityAt) }),
      i18n.t('documents.details.queueDuration', { value: formatDuration(attempt.queueElapsedMs) }),
      i18n.t('documents.details.totalDuration', { value: formatDuration(attempt.totalElapsedMs) }),
      `${i18n.t('documents.details.accounting')} ${attempt.summary.accountingStatus}`,
      `${i18n.t('documents.details.totalCost')} ${formatMoney(attempt.summary.totalEstimatedCost, attempt.summary.currency)}`,
      attempt.summary.settledEstimatedCost !== null
        ? i18n.t('documents.details.settledAmount', {
            value: formatMoney(attempt.summary.settledEstimatedCost, attempt.summary.currency),
          })
        : null,
      attempt.summary.inFlightStageCount > 0
        ? i18n.t('documents.details.liveStages', { count: attempt.summary.inFlightStageCount })
        : null,
      attempt.summary.missingStageCount > 0
        ? i18n.t('documents.details.missingStages', { count: attempt.summary.missingStageCount })
        : null,
    ].filter((value): value is string => Boolean(value)),
    benchmarks: attempt.benchmarks.map((benchmark, index) => ({
      key: `${String(attempt.attemptNo)}-${benchmark.stage}-${benchmark.startedAt}-${String(index)}`,
      tone:
        benchmark.status === 'failed'
          ? ('failed' as DocumentStatus)
          : benchmark.status === 'completed' || benchmark.status === 'skipped'
            ? ('ready' as DocumentStatus)
            : ('processing' as DocumentStatus),
      title: stageLabel(benchmark.stage),
      subtitle: [
        benchmark.status,
        benchmark.providerKind,
        benchmark.modelName,
      ]
        .filter(Boolean)
        .join(' · '),
      timing: [
        formatDate(benchmark.startedAt),
        benchmark.finishedAt ? formatDate(benchmark.finishedAt) : null,
        formatDuration(benchmark.elapsedMs),
      ]
        .filter(Boolean)
        .join(' · '),
      message: benchmark.message,
      accounting: benchmark.accounting
        ? [
            benchmark.accounting.pricingStatus,
            benchmark.accounting.attributionSource ?? null,
            formatMoney(
              benchmark.accounting.settledEstimatedCost ??
                benchmark.accounting.inFlightEstimatedCost ??
                benchmark.accounting.estimatedCost,
              benchmark.accounting.currency,
            ),
          ]
            .filter(Boolean)
            .join(' · ')
        : null,
    })),
  }))
})

const extractedPreview = computed(() => {
  const preview = props.detail?.extractedStats.previewText?.trim() ?? ''
  return preview.length > 0 ? preview : null
})

const showOpenInGraph = computed(() => Boolean(props.detail?.graphNodeId))
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
const deleteInProgress = computed(
  () =>
    props.detail?.mutation.kind === 'delete' &&
    (props.detail.mutation.status === 'accepted' || props.detail.mutation.status === 'reconciling'),
)
const disableReprocess = computed(
  () =>
    props.detail?.status === 'processing' ||
    props.detail?.status === 'queued' ||
    mutationLocked.value ||
    props.detail?.activityStatus === 'retrying',
)
</script>

<template>
  <div
    v-if="props.open"
    class="rr-documents-drawer"
  >
    <div
      class="rr-documents-drawer__backdrop"
      @click="emit('close')"
    />

    <aside class="rr-documents-drawer__panel">
      <header class="rr-documents-drawer__head">
        <div>
          <span class="rr-documents-drawer__eyebrow">{{ $t('documents.headers.document') }}</span>
          <h2 v-if="props.detail">{{ props.detail.fileName }}</h2>
          <h2 v-else>{{ $t('documents.actions.details') }}</h2>
          <p v-if="props.detail">{{ [props.workspaceName ?? '—', props.detail.libraryName].join(' / ') }}</p>
        </div>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('close')"
        >
          {{ $t('dialogs.close') }}
        </button>
      </header>

      <div
        v-if="props.loading"
        class="rr-page-card rr-documents-drawer__section"
      >
        <p>{{ $t('documents.loadingDetail') }}</p>
      </div>

      <div
        v-else-if="props.error"
        class="rr-page-card rr-documents-drawer__section"
      >
        <p>{{ props.error }}</p>
      </div>

      <template v-else-if="props.detail">
        <section class="rr-page-card rr-documents-drawer__section">
          <p>{{ props.detail.summary }}</p>
          <div class="rr-documents-drawer__pill-row">
            <StatusPill
              :tone="props.detail.status"
              :label="statusLabel(props.detail.status)"
            />
            <StatusPill
              v-if="props.detail.mutation.status"
              :tone="mutationTone(props.detail.mutation.status)"
              :label="mutationLabel(props.detail.mutation.status)!"
            />
            <StatusPill
              v-if="readableTruth"
              :tone="readableTruth.tone"
              :label="readableTruth.label"
            />
            <StatusPill
              v-if="settledTruth"
              :tone="settledTruth.tone"
              :label="settledTruth.label"
            />
          </div>
          <div class="rr-documents-drawer__truth-grid">
            <article
              v-if="readableTruth"
              class="rr-documents-drawer__soft-card"
            >
              <span>{{ $t('documents.details.readableTruth') }}</span>
              <strong>{{ readableTruth.label }}</strong>
              <p>{{ readableTruth.note }}</p>
            </article>
            <article
              v-if="settledTruth"
              class="rr-documents-drawer__soft-card"
            >
              <span>{{ $t('documents.details.settledTruth') }}</span>
              <strong>{{ settledTruth.label }}</strong>
              <p>{{ settledTruth.note }}</p>
            </article>
          </div>
        </section>

        <section
          v-if="readinessCards.length"
          class="rr-page-card rr-documents-drawer__section"
        >
          <h3>{{ $t('documents.details.knowledgeReadiness') }}</h3>
          <div class="rr-documents-drawer__truth-grid">
            <article
              v-for="card in readinessCards"
              :key="card.key"
              class="rr-documents-drawer__soft-card"
            >
              <span>{{ card.label }}</span>
              <strong>{{ card.value }}</strong>
              <p>{{ card.note }}</p>
            </article>
          </div>
        </section>

        <section class="rr-documents-drawer__summary-grid">
          <DocumentSummaryCard
            v-for="card in summaryCards"
            :key="card.key"
            :tone="card.tone"
            :value="card.value"
            :label="card.label"
          />
        </section>

        <section class="rr-page-card rr-documents-drawer__section">
          <h3>{{ $t('documents.details.overview') }}</h3>
          <dl class="rr-documents-drawer__meta-grid">
            <template
              v-for="item in metadataRows"
              :key="item.key"
            >
              <dt>{{ item.label }}</dt>
              <dd>{{ item.value }}</dd>
            </template>
          </dl>
        </section>

        <section
          v-if="extractedPreview"
          class="rr-page-card rr-documents-drawer__section"
        >
          <div class="rr-documents-drawer__section-head">
            <h3>{{ $t('documents.details.readableText') }}</h3>
            <button
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              :disabled="!props.detail.canDownloadText"
              @click="emit('downloadText', props.detail.id)"
            >
              {{ $t('documents.details.downloadText') }}
            </button>
          </div>
          <p class="rr-documents-drawer__preview-text">{{ extractedPreview }}</p>
        </section>

        <section class="rr-page-card rr-documents-drawer__section">
          <h3>{{ $t('documents.details.mutationSection') }}</h3>
          <dl class="rr-documents-drawer__meta-grid">
            <template
              v-for="item in mutationRows"
              :key="item.key"
            >
              <dt>{{ item.label }}</dt>
              <dd>{{ item.value }}</dd>
            </template>
          </dl>
        </section>

        <section
          v-if="revisionTimeline.length"
          class="rr-page-card rr-documents-drawer__section"
        >
          <h3>{{ $t('documents.details.revisionHistory') }}</h3>
          <ol class="rr-documents-drawer__timeline">
            <li
              v-for="revision in revisionTimeline"
              :key="revision.key"
              class="rr-documents-drawer__timeline-item"
            >
              <strong>{{ revision.title }}</strong>
              <span>{{ revision.subtitle }}</span>
              <span>{{ revision.timestamp }}</span>
              <p v-if="revision.body">{{ revision.body }}</p>
            </li>
          </ol>
        </section>

        <section
          v-if="attemptSections.length"
          class="rr-page-card rr-documents-drawer__section"
        >
          <h3>{{ $t('documents.details.attemptStages') }}</h3>
          <article
            v-for="attempt in attemptSections"
            :key="attempt.key"
            class="rr-documents-drawer__attempt-card"
          >
            <header class="rr-documents-drawer__attempt-head">
              <strong>{{ attempt.heading }}</strong>
              <span>{{ attempt.subtitle }}</span>
            </header>
            <ul class="rr-documents-drawer__attempt-meta">
              <li
                v-for="line in attempt.meta"
                :key="line"
              >
                {{ line }}
              </li>
            </ul>
            <div
              v-if="attempt.benchmarks.length"
              class="rr-documents-drawer__benchmark-list"
            >
              <article
                v-for="benchmark in attempt.benchmarks"
                :key="benchmark.key"
                class="rr-documents-drawer__benchmark-card"
              >
                <div class="rr-documents-drawer__benchmark-head">
                  <StatusPill
                    :tone="benchmark.tone"
                    :label="benchmark.title"
                  />
                  <span>{{ benchmark.subtitle }}</span>
                </div>
                <p>{{ benchmark.timing }}</p>
                <p v-if="benchmark.accounting">{{ benchmark.accounting }}</p>
                <p v-if="benchmark.message">{{ benchmark.message }}</p>
              </article>
            </div>
          </article>
        </section>

        <footer class="rr-documents-drawer__actions">
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
            v-if="showOpenInGraph && props.detail.graphNodeId"
            class="rr-button rr-button--ghost"
            type="button"
            @click="emit('openInGraph', props.detail.graphNodeId)"
          >
            {{ $t('documents.details.openInGraph') }}
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
        </footer>
      </template>
    </aside>
  </div>
</template>
