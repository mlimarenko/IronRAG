<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  DocumentCollectionDiagnostics,
  DocumentRow,
  DocumentStatus,
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
const { humanizeToken, shortIdentifier } = useDisplayFormatters()

function formatDate(value: string): string {
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
  return i18n.te(key) ? i18n.t(key) : humanizeToken(stage)
}

function statusLabel(status: string): string {
  const key = `documents.status.${status}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(status)
}

function activityLabel(status: string): string {
  const key = `documents.activity.${status}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(status)
}

function revisionKindLabel(kind: string | null): string | null {
  if (!kind) {
    return null
  }
  const key = `documents.revision.kind.${kind}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(kind)
}

function mutationLabel(status: string | null): string | null {
  if (!status) {
    return null
  }
  const key = `documents.mutation.status.${status}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(status)
}

function mutationKindLabel(kind: string | null): string | null {
  if (!kind) {
    return null
  }
  const key = `documents.mutation.kind.${kind}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(kind)
}

function truthTone(kind: 'readable' | 'settled', row: DocumentRow): DocumentStatus {
  if (kind === 'readable') {
    if (isReadable(row)) {
      return row.status === 'ready_no_graph' ? 'ready_no_graph' : 'ready'
    }
    return row.status === 'failed' ? 'failed' : row.status === 'queued' ? 'queued' : 'processing'
  }
  if (row.status === 'failed' && !isSettling(row)) {
    return 'failed'
  }
  return isSettling(row) ? 'processing' : 'ready'
}

function isReadable(row: DocumentRow): boolean {
  return row.status === 'ready' || row.status === 'ready_no_graph'
}

function hasPendingMutation(row: DocumentRow): boolean {
  return row.mutation.status === 'accepted' || row.mutation.status === 'reconciling'
}

function isSettling(row: DocumentRow): boolean {
  return (
    hasPendingMutation(row) ||
    row.activityStatus === 'queued' ||
    row.activityStatus === 'active' ||
    row.activityStatus === 'blocked' ||
    row.activityStatus === 'retrying' ||
    row.activityStatus === 'stalled' ||
    row.inFlightStageCount > 0 ||
    row.missingStageCount > 0
  )
}

function readableTruthLabel(row: DocumentRow): string {
  if (row.status === 'ready_no_graph') {
    return i18n.t('documents.details.truth.readableWithGraphCatchUp')
  }
  if (isReadable(row)) {
    return i18n.t('documents.details.truth.readable')
  }
  if (row.status === 'failed') {
    return i18n.t('documents.details.truth.readableUnavailable')
  }
  return i18n.t('documents.details.truth.notReadableYet')
}

function readableTruthNote(row: DocumentRow): string | null {
  if (isReadable(row)) {
    return row.chunkCount !== null
      ? i18n.t('documents.details.truthNotes.chunksReady', { count: row.chunkCount })
      : statusLabel(row.status)
  }
  if (row.status === 'failed') {
    return row.mutation.warning ?? activityLabel(row.activityStatus)
  }
  return stageLabel(row.stage)
}

function settledTruthLabel(row: DocumentRow): string {
  if (row.status === 'failed' && !isSettling(row)) {
    return i18n.t('documents.details.truth.settledWithFailure')
  }
  return isSettling(row)
    ? i18n.t('documents.details.truth.settling')
    : i18n.t('documents.details.truth.settled')
}

function settledTruthNote(row: DocumentRow): string | null {
  const notes = [
    row.inFlightStageCount > 0
      ? i18n.t('documents.details.liveStages', { count: row.inFlightStageCount })
      : null,
    row.missingStageCount > 0
      ? i18n.t('documents.details.missingStages', { count: row.missingStageCount })
      : null,
    row.settledEstimatedCost !== null
      ? i18n.t('documents.details.settledAmount', {
          value: formatMoney(row.settledEstimatedCost, row.currency),
        })
      : null,
  ].filter((value): value is string => Boolean(value))

  if (notes.length > 0) {
    return notes.join(' · ')
  }
  return row.lastActivityAt
    ? i18n.t('documents.details.lastActivityAt', { value: formatDate(row.lastActivityAt) })
    : null
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
      maximumFractionDigits: 4,
    }).format(value)
  } catch {
    return `${value.toFixed(4)} ${normalizedCurrency}`
  }
}

function revisionLabel(row: DocumentRow): string {
  if (!row.activeRevisionNo) {
    return '—'
  }
  const kind = revisionKindLabel(row.activeRevisionKind)
  return kind ? `#${String(row.activeRevisionNo)} · ${kind}` : `#${String(row.activeRevisionNo)}`
}

function compactIdentifier(value: string | null): string | null {
  if (!value) {
    return null
  }
  return shortIdentifier(value)
}

function mutationSummary(row: DocumentRow): string {
  if (!row.mutation.kind && !row.mutation.status) {
    return '—'
  }
  return [mutationKindLabel(row.mutation.kind), mutationLabel(row.mutation.status)]
    .filter(Boolean)
    .join(' · ')
}
</script>

<template>
  <section class="rr-page-card rr-documents__table">
    <div class="rr-documents__table-scroll">
      <table>
        <colgroup>
          <col class="rr-documents__col-file">
          <col class="rr-documents__col-revision">
          <col class="rr-documents__col-stage">
          <col class="rr-documents__col-status">
          <col class="rr-documents__col-status">
          <col class="rr-documents__col-progress">
          <col class="rr-documents__col-actions">
        </colgroup>
        <thead>
          <tr>
            <th class="rr-documents__col-file">{{ $t('documents.headers.document') }}</th>
            <th class="rr-documents__col-revision">{{ $t('documents.headers.revision') }}</th>
            <th class="rr-documents__col-stage">{{ $t('documents.headers.latestAttempt') }}</th>
            <th class="rr-documents__col-status">{{ $t('documents.headers.status') }}</th>
            <th class="rr-documents__col-status">{{ $t('documents.headers.truth') }}</th>
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
              <div class="rr-documents__file-cell">
                <strong>{{ row.fileName }}</strong>
                <span class="rr-documents__file-meta">
                  {{ [row.fileType, row.fileSizeLabel, formatDate(row.uploadedAt)].join(' · ') }}
                </span>
              </div>
            </td>

            <td class="rr-documents__cell-revision">
              <div class="rr-documents__meta-stack">
                <strong>{{ revisionLabel(row) }}</strong>
                <span v-if="row.activeRevisionId">
                  {{ `rev ${compactIdentifier(row.activeRevisionId)}` }}
                </span>
                <span v-else-if="row.logicalDocumentId">
                  {{ `doc ${compactIdentifier(row.logicalDocumentId)}` }}
                </span>
              </div>
            </td>

            <td class="rr-documents__cell-stage">
              <div class="rr-documents__meta-stack">
                <strong>{{ row.latestAttemptNo > 0 ? `#${String(row.latestAttemptNo)}` : '—' }}</strong>
                <span>{{ stageLabel(row.stage) }}</span>
                <span>{{ activityLabel(row.activityStatus) }}</span>
              </div>
            </td>

            <td class="rr-documents__cell-status">
              <div class="rr-documents__status-cell">
                <div class="rr-documents__status-cell--pills">
                  <StatusPill
                    :tone="row.status"
                    :label="statusLabel(row.status)"
                  />
                  <StatusPill
                    v-if="row.mutation.status"
                    :tone="row.mutation.status === 'failed' ? 'failed' : 'processing'"
                    :label="mutationLabel(row.mutation.status)!"
                  />
                </div>
                <span class="rr-documents__cell-note">
                  {{ mutationSummary(row) }}
                </span>
              </div>
            </td>

            <td class="rr-documents__cell-status">
              <div class="rr-documents__status-cell">
                <div class="rr-documents__status-cell--pills">
                  <StatusPill
                    :tone="truthTone('readable', row)"
                    :label="readableTruthLabel(row)"
                  />
                  <StatusPill
                    :tone="truthTone('settled', row)"
                    :label="settledTruthLabel(row)"
                  />
                </div>
                <span class="rr-documents__cell-note">
                  {{ readableTruthNote(row) ?? settledTruthNote(row) ?? '—' }}
                </span>
                <span
                  v-if="readableTruthNote(row) && settledTruthNote(row)"
                  class="rr-documents__cell-note"
                >
                  {{ settledTruthNote(row) }}
                </span>
              </div>
            </td>

            <td class="rr-documents__cell-progress">
              <DocumentProgressCell
                :progress-percent="row.progressPercent"
                :status="row.status"
                :activity-status="row.activityStatus"
                :attempt-no="row.latestAttemptNo"
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
