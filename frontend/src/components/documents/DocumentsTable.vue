<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import type {
  DocumentActivityStatus,
  DocumentCollectionDiagnostics,
  DocumentRow,
} from 'src/models/ui/documents'
import DocumentProgressCell from './DocumentProgressCell.vue'
import DocumentRowActions from './DocumentRowActions.vue'

const props = defineProps<{
  rows: DocumentRow[]
  selectedId?: string | null
  diagnostics?: DocumentCollectionDiagnostics | null
}>()

const emit = defineEmits<{
  detail: [id: string]
  append: [id: string]
  replace: [id: string]
  retry: [id: string]
  remove: [id: string]
}>()

const i18n = useI18n()

function formatDate(value: string): string {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString()
}

function formatCompactDate(value: string): string {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function formatShortDateTime(value: string): string {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString(undefined, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function stageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  return i18n.te(key) ? i18n.t(key) : stage
}

function revisionKindLabel(kind: string | null): string | null {
  if (!kind) {
    return null
  }
  const key = `documents.revision.kind.${kind}`
  return i18n.te(key) ? i18n.t(key) : kind
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

function mutationLabel(status: string | null): string | null {
  if (!status || status === 'completed') {
    return null
  }
  const key = `documents.mutation.status.${status}`
  return i18n.te(key) ? i18n.t(key) : status
}

function mutationTone(status: string | null): DocumentRow['status'] {
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

function activityHint(row: DocumentRow): string | null {
  if (row.stalledReason) {
    return row.stalledReason
  }
  if (row.lastActivityAt) {
    return i18n.t('documents.lastActivityAt', { value: formatDate(row.lastActivityAt) })
  }
  return null
}

function activityNote(row: DocumentRow): string | null {
  if (row.stalledReason) {
    return row.stalledReason
  }
  if (row.lastActivityAt) {
    return formatShortDateTime(row.lastActivityAt)
  }
  return null
}

function formatInlineCost(value: number | null, currency: string | null): string | null {
  if (value === null) {
    return null
  }
  const normalizedCurrency = currency ?? 'USD'
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: normalizedCurrency,
      maximumFractionDigits: 4,
    }).format(value)
  } catch {
    return `${value.toFixed(4)} ${normalizedCurrency}`
  }
}

const averageEstimatedCost = computed(() => {
  const values = props.rows
    .map((row) => row.totalEstimatedCost)
    .filter((value): value is number => value !== null)

  if (!values.length) {
    return null
  }

  return values.reduce((total, value) => total + value, 0) / values.length
})

const bottleneckFormat = computed<DocumentCollectionDiagnostics['perFormat'][number] | null>(() => {
  const formats = props.diagnostics?.perFormat ?? []
  return [...formats].sort((left, right) => {
    const leftValue = left.bottleneckAvgElapsedMs ?? -1
    const rightValue = right.bottleneckAvgElapsedMs ?? -1
    return rightValue - leftValue
  })[0] ?? null
})

const diagnosticsSummary = computed(() => {
  if (!props.diagnostics) {
    return null
  }
  if (bottleneckFormat.value?.bottleneckStage) {
    return i18n.t('documents.diagnostics.tableSummaryWithBottleneck', {
      active: props.diagnostics.activeBacklogCount,
      fileType: bottleneckFormat.value.fileType,
      stage: stageLabel(bottleneckFormat.value.bottleneckStage),
    })
  }
  return i18n.t('documents.diagnostics.tableSummary', {
    active: props.diagnostics.activeBacklogCount,
  })
})

function accountingFillPercent(status: string): number {
  switch (status) {
    case 'priced':
      return 100
    case 'partial':
      return 56
    default:
      return 0
  }
}

function accountingVisualTone(row: DocumentRow): 'low' | 'mid' | 'high' | 'none' {
  if (row.totalEstimatedCost === null) {
    return 'none'
  }

  const average = averageEstimatedCost.value
  if (average === null || average <= 0) {
    return 'mid'
  }

  if (row.totalEstimatedCost <= average * 0.82) {
    return 'low'
  }
  if (row.totalEstimatedCost >= average * 1.18) {
    return 'high'
  }
  return 'mid'
}

function accountingDisplay(row: DocumentRow): string {
  return formatInlineCost(row.totalEstimatedCost, row.currency) ?? accountingLabel(row.accountingStatus)
}

function accountingTitle(row: DocumentRow): string | null {
  return cellTitle([
    accountingDisplay(row),
    accountingLabel(row.accountingStatus),
    averageEstimatedCost.value !== null
      ? i18n.t('documents.averageCostHint', {
          value: formatInlineCost(averageEstimatedCost.value, row.currency ?? 'USD') ?? '—',
        })
      : null,
  ])
}

function contributionSummary(row: DocumentRow): string | null {
  if (row.status !== 'ready' && row.status !== 'ready_no_graph') {
    return null
  }
  const chunks = row.chunkCount ?? 0
  const nodes = row.graphNodeCount ?? 0
  const edges = row.graphEdgeCount ?? 0
  return i18n.t('documents.contributionSummary', { chunks, nodes, edges })
}

function showActivityPill(row: DocumentRow): boolean {
  return !['ready', 'failed'].includes(row.activityStatus)
}

function showPrimaryStatusPill(row: DocumentRow): boolean {
  return !['queued', 'processing'].includes(row.status)
}

function cellTitle(parts: (string | null | undefined)[]): string | null {
  const value = parts
    .map((part) => part?.trim())
    .filter(Boolean)
    .join(' · ')

  return value || null
}
</script>

<template>
  <section class="rr-page-card rr-documents__table">
    <header
      v-if="diagnosticsSummary"
      class="rr-documents__table-header"
    >
      <strong>{{ $t('documents.diagnostics.tableTitle') }}</strong>
      <span>{{ diagnosticsSummary }}</span>
    </header>
    <div class="rr-documents__table-scroll">
      <table>
        <colgroup>
          <col class="rr-documents__col-file">
          <col class="rr-documents__col-type">
          <col class="rr-documents__col-size">
          <col class="rr-documents__col-uploaded">
          <col class="rr-documents__col-revision">
          <col class="rr-documents__col-attempt">
          <col class="rr-documents__col-stage">
          <col class="rr-documents__col-accounting">
          <col class="rr-documents__col-status">
          <col class="rr-documents__col-progress">
          <col class="rr-documents__col-actions">
        </colgroup>
        <thead>
          <tr>
            <th class="rr-documents__col-file">{{ $t('documents.headers.fileName') }}</th>
            <th class="rr-documents__col-type">{{ $t('documents.headers.type') }}</th>
            <th class="rr-documents__col-size">{{ $t('documents.headers.size') }}</th>
            <th class="rr-documents__col-uploaded">{{ $t('documents.headers.uploaded') }}</th>
            <th class="rr-documents__col-revision">{{ $t('documents.headers.revision') }}</th>
            <th class="rr-documents__col-attempt">{{ $t('documents.headers.attempt') }}</th>
            <th class="rr-documents__col-stage">{{ $t('documents.headers.stage') }}</th>
            <th class="rr-documents__col-accounting">{{ $t('documents.headers.accounting') }}</th>
            <th class="rr-documents__col-status">{{ $t('documents.headers.status') }}</th>
            <th class="rr-documents__col-progress">{{ $t('documents.headers.progress') }}</th>
            <th class="rr-documents__col-actions">{{ $t('documents.headers.actions') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in props.rows"
            :key="row.id"
            :class="{ 'is-selected': row.id === props.selectedId, 'is-clickable': row.detailAvailable }"
            @click="row.detailAvailable && emit('detail', row.id)"
          >
            <td class="rr-documents__cell-file">
              <div
                class="rr-documents__file-cell"
                :title="cellTitle([row.fileName, contributionSummary(row)]) ?? undefined"
              >
                <strong>{{ row.fileName }}</strong>
                <span
                  v-if="contributionSummary(row)"
                  class="rr-documents__file-meta"
                >
                  {{ contributionSummary(row) }}
                </span>
              </div>
            </td>
            <td
              class="rr-documents__cell-type"
              :title="row.fileType"
            >
              {{ row.fileType }}
            </td>
            <td
              class="rr-documents__cell-size"
              :title="row.fileSizeLabel"
            >
              {{ row.fileSizeLabel }}
            </td>
            <td
              class="rr-documents__cell-uploaded"
              :title="formatDate(row.uploadedAt)"
            >
              <div class="rr-documents__meta-stack">
                <strong>{{ formatCompactDate(row.uploadedAt) }}</strong>
              </div>
            </td>
            <td
              class="rr-documents__cell-revision"
              :title="row.activeRevisionNo ? `#${row.activeRevisionNo}` : '—'"
            >
              {{ row.activeRevisionNo ? `#${row.activeRevisionNo}` : '—' }}
            </td>
            <td class="rr-documents__cell-attempt">
              <span :title="row.latestAttemptNo > 0 ? `#${row.latestAttemptNo}` : '—'">
                {{ row.latestAttemptNo > 0 ? `#${row.latestAttemptNo}` : '—' }}
              </span>
            </td>
            <td
              class="rr-documents__cell-stage"
              :title="stageLabel(row.stage)"
            >
              <div class="rr-documents__meta-stack">
                <strong>{{ stageLabel(row.stage) }}</strong>
                <span v-if="revisionKindLabel(row.activeRevisionKind)">
                  {{ revisionKindLabel(row.activeRevisionKind) }}
                </span>
              </div>
            </td>
            <td class="rr-documents__cell-accounting">
              <div
                class="rr-documents__status-cell"
                :title="accountingTitle(row) ?? undefined"
              >
                <span
                  class="rr-documents__cost-pill"
                  :class="[
                    `is-${accountingVisualTone(row)}`,
                    `is-${row.accountingStatus}`,
                  ]"
                  :style="{ '--rr-cost-fill': `${accountingFillPercent(row.accountingStatus)}%` }"
                >
                  <span class="rr-documents__cost-pill-copy">
                    {{ accountingDisplay(row) }}
                  </span>
                </span>
              </div>
            </td>
            <td class="rr-documents__cell-status">
              <div
                class="rr-documents__status-cell"
                :title="
                  cellTitle([
                    activityLabel(row.activityStatus),
                    statusLabel(row.status),
                    mutationLabel(row.mutation.status),
                    activityHint(row),
                  ]) ?? undefined
                "
              >
                <div class="rr-documents__status-cell--pills">
                  <StatusPill
                    v-if="showActivityPill(row)"
                    :tone="activityTone(row.activityStatus)"
                    :label="activityLabel(row.activityStatus)"
                  />
                  <StatusPill
                    v-if="showPrimaryStatusPill(row)"
                    :tone="row.status"
                    :label="statusLabel(row.status)"
                  />
                  <StatusPill
                    v-if="mutationLabel(row.mutation.status)"
                    :tone="mutationTone(row.mutation.status)"
                    :label="mutationLabel(row.mutation.status)!"
                  />
                  <span
                    v-if="!showActivityPill(row) && !showPrimaryStatusPill(row) && !mutationLabel(row.mutation.status)"
                  >—</span>
                </div>
                <span
                  v-if="activityNote(row)"
                  class="rr-documents__cell-note"
                >
                  {{ activityNote(row) }}
                </span>
              </div>
            </td>
            <td class="rr-documents__cell-progress">
              <DocumentProgressCell
                :progress-percent="row.progressPercent"
                :status="row.status"
                :activity-status="row.activityStatus"
              />
            </td>
            <td class="rr-documents__cell-actions">
              <DocumentRowActions
                :can-append="row.canAppend"
                :can-replace="row.canReplace"
                :can-retry="row.canRetry"
                :can-remove="row.canRemove"
                :detail-available="row.detailAvailable"
                :activity-status="row.activityStatus"
                :mutation-kind="row.mutation.kind"
                :mutation-status="row.mutation.status"
                @detail="emit('detail', row.id)"
                @append="emit('append', row.id)"
                @replace="emit('replace', row.id)"
                @retry="emit('retry', row.id)"
                @remove="emit('remove', row.id)"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
