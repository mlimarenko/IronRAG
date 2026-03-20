<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import DocumentSummaryCard from './DocumentSummaryCard.vue'
import type {
  DocumentActivityStatus,
  DocumentCollectionDiagnostics,
  DocumentDetail,
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

const resolvedDiagnostics = computed(() => {
  return props.detail?.collectionDiagnostics ?? props.libraryDiagnostics ?? null
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
      label: detail.status === 'ready_no_graph' ? 'Readable with graph catch-up' : 'Readable',
      note:
        detail.extractedStats.previewTruncated || detail.status === 'ready_no_graph'
          ? 'Text is already usable while downstream graph work is still catching up.'
          : 'Current extracted text is ready for read and search flows.',
    }
  }

  return {
    tone: detail.status === 'failed' ? 'failed' : detail.status === 'queued' ? 'queued' : 'processing',
    label: detail.status === 'failed' ? 'Readable state unavailable' : 'Not readable yet',
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
      label: 'Residual failure',
      note: terminal.residualReason ?? detail.errorMessage ?? 'Downstream work settled in failure.',
    }
  }

  if (settlement?.isFullySettled || terminal?.terminalState === 'fully_settled') {
    return {
      tone: 'ready' as DocumentStatus,
      label: 'Settled',
      note: terminal?.settledAt
        ? `Settled at ${formatDate(terminal.settledAt)}`
        : settlement?.settledAt
          ? `Settled at ${formatDate(settlement.settledAt)}`
          : 'No in-flight work remains for this document context.',
    }
  }

  if (activeWork) {
    return {
      tone: 'processing' as DocumentStatus,
      label: 'Settling',
      note: [
        detail.inFlightStageCount > 0 ? `${detail.inFlightStageCount} live stage(s)` : null,
        detail.missingStageCount > 0 ? `${detail.missingStageCount} missing stage(s)` : null,
      ]
        .filter(Boolean)
        .join(' · ') || 'Mutation or ingest work is still in progress.',
    }
  }

  return {
    tone: detail.status === 'failed' ? ('failed' as DocumentStatus) : ('ready_no_graph' as DocumentStatus),
    label: detail.status === 'failed' ? 'Failed' : 'Open',
    note: 'Readable state is known, but final settlement has not been confirmed yet.',
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
      label: 'Active revision',
    },
    {
      key: 'attempt',
      tone: detail.status,
      value: detail.latestAttemptNo > 0 ? `#${String(detail.latestAttemptNo)}` : '—',
      label: 'Latest attempt',
    },
    {
      key: 'stage',
      tone: detail.status,
      value: stageLabel(detail.stage),
      label: 'Current stage',
    },
    {
      key: 'cost',
      tone: summaryTone(settledTruth.value?.tone ?? detail.status),
      value:
        detail.settledEstimatedCost !== null
          ? formatMoney(detail.settledEstimatedCost, detail.currency)
          : formatMoney(detail.totalEstimatedCost, detail.currency),
      label: detail.settledEstimatedCost !== null ? 'Settled cost' : 'Estimated cost',
    },
  ]
})

const metadataRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    { key: 'workspace', label: 'Workspace', value: props.workspaceName ?? '—' },
    { key: 'library', label: 'Library', value: detail.libraryName },
    { key: 'uploaded', label: 'Uploaded', value: formatDate(detail.uploadedAt) },
    { key: 'fileType', label: 'File type', value: detail.fileType },
    { key: 'fileSize', label: 'File size', value: detail.fileSizeLabel },
    { key: 'requestedBy', label: 'Requested by', value: detail.requestedBy ?? '—' },
    { key: 'accounting', label: 'Accounting', value: detail.accountingStatus },
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
      label: 'Operation',
      value: mutationKindLabel(detail.mutation.kind) ?? 'No active mutation',
    },
    {
      key: 'status',
      label: 'Status',
      value: mutationLabel(detail.mutation.status) ?? 'Idle',
    },
    {
      key: 'warning',
      label: 'Warning',
      value: detail.mutation.warning ?? '—',
    },
    {
      key: 'error',
      label: 'Error',
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
      revision.isActive ? 'Active head' : null,
    ]
      .filter(Boolean)
      .join(' · '),
    timestamp: [
      `Accepted ${formatDate(revision.acceptedAt)}`,
      revision.activatedAt ? `Activated ${formatDate(revision.activatedAt)}` : null,
      revision.supersededAt ? `Superseded ${formatDate(revision.supersededAt)}` : null,
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
    heading: `Attempt #${String(attempt.attemptNo)}`,
    subtitle: [
      attemptKindLabel(attempt.attemptKind),
      attempt.revisionNo ? `Revision #${String(attempt.revisionNo)}` : null,
      attempt.status,
      activityLabel(attempt.activityStatus),
    ]
      .filter(Boolean)
      .join(' · '),
    meta: [
      `Last activity ${formatDate(attempt.lastActivityAt)}`,
      `Queue ${formatDuration(attempt.queueElapsedMs)}`,
      `Total ${formatDuration(attempt.totalElapsedMs)}`,
      `Accounting ${attempt.summary.accountingStatus}`,
      `Cost ${formatMoney(attempt.summary.totalEstimatedCost, attempt.summary.currency)}`,
      attempt.summary.settledEstimatedCost !== null
        ? `Settled ${formatMoney(attempt.summary.settledEstimatedCost, attempt.summary.currency)}`
        : null,
      attempt.summary.inFlightStageCount > 0
        ? `${attempt.summary.inFlightStageCount} live stage(s)`
        : null,
      attempt.summary.missingStageCount > 0
        ? `${attempt.summary.missingStageCount} missing stage(s)`
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
          <span class="rr-documents-drawer__eyebrow">Document</span>
          <h2 v-if="props.detail">{{ props.detail.fileName }}</h2>
          <h2 v-else>{{ $t('documents.actions.details') }}</h2>
          <p v-if="props.detail">{{ [props.workspaceName ?? '—', props.detail.libraryName].join(' / ') }}</p>
        </div>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('close')"
        >
          {{ $t('common.close') }}
        </button>
      </header>

      <div
        v-if="props.loading"
        class="rr-page-card rr-documents-drawer__section"
      >
        <p>{{ $t('common.loading') }}</p>
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
              <span>Readable truth</span>
              <strong>{{ readableTruth.label }}</strong>
              <p>{{ readableTruth.note }}</p>
            </article>
            <article
              v-if="settledTruth"
              class="rr-documents-drawer__soft-card"
            >
              <span>Settled truth</span>
              <strong>{{ settledTruth.label }}</strong>
              <p>{{ settledTruth.note }}</p>
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
          <h3>Overview</h3>
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
            <h3>Readable text</h3>
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
          <h3>Mutation</h3>
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
          <h3>Revision history</h3>
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
          <h3>Attempt stages</h3>
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
