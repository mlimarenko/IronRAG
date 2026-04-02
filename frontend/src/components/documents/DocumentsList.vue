<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DocumentRowSummary, DocumentsSortField } from 'src/models/ui/documents'

const props = defineProps<{
  rows: DocumentRowSummary[]
  selectedId?: string | null
  sortField: DocumentsSortField
  sortDirection: 'asc' | 'desc'
}>()

const emit = defineEmits<{
  detail: [id: string]
  retry: [id: string]
  sort: [field: DocumentsSortField]
}>()

const i18n = useI18n()
const t = (key: string, named?: Record<string, unknown>) => i18n.t(key, named ?? {})
const te = (key: string) => i18n.te(key)
const formatters = useDisplayFormatters()
const formatCompactDateTime = (value: string | null | undefined) =>
  formatters.formatCompactDateTime(value ?? null)
const formatDateTime = (value: string | null | undefined) =>
  formatters.formatDateTime(value ?? null)
const showTypeColumn = computed(() => {
  const visibleTypes = new Set(
    props.rows.map((row) => row.fileType).filter((value) => value.trim().length > 0),
  )
  return visibleTypes.size > 1
})

const showCostColumn = computed(() =>
  props.rows.some((row) => Boolean(row.costLabel && row.costLabel.trim().length > 0)),
)
function hasDetailTarget(row: DocumentRowSummary): boolean {
  return row.detailAvailable && row.id.trim().length > 0
}

function openDetail(row: DocumentRowSummary): void {
  if (!hasDetailTarget(row)) {
    return
  }
  emit('detail', row.id)
}

function compactMeta(row: DocumentRowSummary): string {
  return [
    showTypeColumn.value ? row.fileType : '',
    row.fileSizeLabel,
    formatCompactDateTime(row.uploadedAt),
  ]
    .filter(Boolean)
    .join(' · ')
}

function isSortActive(field: DocumentsSortField): boolean {
  return props.sortField === field
}

function ariaSort(field: DocumentsSortField): 'none' | 'ascending' | 'descending' {
  if (!isSortActive(field)) {
    return 'none'
  }
  return props.sortDirection === 'asc' ? 'ascending' : 'descending'
}

function sortIndicator(field: DocumentsSortField): string {
  if (!isSortActive(field)) {
    return '↕'
  }
  return props.sortDirection === 'asc' ? '↑' : '↓'
}

function isQueued(row: DocumentRowSummary): boolean {
  return row.status === 'queued'
}

function isProcessing(row: DocumentRowSummary): boolean {
  return row.status === 'processing'
}

function readinessKind(row: DocumentRowSummary): string | null {
  return row.preparation?.readinessKind ?? null
}

function isInFlight(row: DocumentRowSummary): boolean {
  return isQueued(row) || isProcessing(row)
}

function isReadable(row: DocumentRowSummary): boolean {
  return readinessKind(row) === 'readable'
}

function isGraphSparse(row: DocumentRowSummary): boolean {
  return readinessKind(row) === 'graph_sparse'
}

function showsProgressFeedback(row: DocumentRowSummary): boolean {
  return isInFlight(row) || isReadable(row) || isGraphSparse(row)
}

function statusTone(
  row: DocumentRowSummary,
): 'queued' | 'processing' | 'readable' | 'graph_sparse' | 'graph_ready' | 'failed' {
  switch (readinessKind(row) ?? row.status) {
    case 'queued':
      return 'queued'
    case 'processing':
      return 'processing'
    case 'readable':
      return 'readable'
    case 'graph_sparse':
      return 'graph_sparse'
    case 'graph_ready':
    case 'ready':
      return 'graph_ready'
    case 'failed':
    default:
      return 'failed'
  }
}

function statusBadgeLabel(row: DocumentRowSummary): string {
  const readiness = readinessKind(row)
  if (readiness) {
    const readinessKey = `documents.readinessKinds.${readiness}`
    if (te(readinessKey)) {
      return t(readinessKey)
    }
  }

  const statusKey = `documents.statuses.${row.status}`
  if (te(statusKey)) {
    return t(statusKey)
  }

  return row.statusLabel
}

const showStatusColumn = computed(() =>
  props.rows.some((row) => {
    const readiness = readinessKind(row)
    return readiness ? readiness !== 'graph_ready' : row.status !== 'ready'
  }),
)

function hasStatusDetail(row: DocumentRowSummary): boolean {
  return Boolean(statusDetailText(row))
}

function liveLabel(row: DocumentRowSummary): string | null {
  if (isQueued(row)) {
    return t('documents.workspace.rowState.queuedEyebrow')
  }

  if (isProcessing(row)) {
    return t('documents.workspace.rowState.processingEyebrow')
  }

  if (isReadable(row)) {
    return t('documents.workspace.rowState.readableEyebrow')
  }

  if (isGraphSparse(row)) {
    return t('documents.workspace.rowState.graphSparseEyebrow')
  }

  return null
}

function statusDetailText(row: DocumentRowSummary): string | null {
  if ((readinessKind(row) ?? row.status) === 'failed' && row.failureMessage) {
    if (row.failureMessage === t('documents.details.failureGeneric')) {
      return t('documents.workspace.rowState.failedGenericDetail')
    }
    return row.failureMessage
  }

  const normalizedStage = row.stageLabel?.trim() ?? ''
  const normalizedStatus = statusBadgeLabel(row).trim()
  if (
    normalizedStage &&
    normalizedStage.localeCompare(normalizedStatus, undefined, { sensitivity: 'accent' }) !== 0
  ) {
    return normalizedStage
  }

  switch (readinessKind(row) ?? row.status) {
    case 'queued':
      return t('documents.workspace.rowState.queuedDetail')
    case 'processing':
      return t('documents.workspace.rowState.processingDetail')
    case 'readable':
      return t('documents.workspace.rowState.readableDetail')
    case 'graph_sparse':
      return t('documents.workspace.rowState.graphSparseDetail')
    case 'failed':
      return t('documents.workspace.rowState.failedDetail')
    default:
      return null
  }
}

function normalizedProgress(row: DocumentRowSummary): number | null {
  if (row.progressPercent === null || !showsProgressFeedback(row)) {
    return null
  }
  const bounded = Math.max(0, Math.min(100, row.progressPercent))
  if (isInFlight(row)) {
    return Math.max(8, Math.min(96, bounded))
  }
  if (isReadable(row)) {
    return Math.max(72, Math.min(84, bounded))
  }
  if (isGraphSparse(row)) {
    return Math.max(86, Math.min(94, bounded))
  }
  return bounded
}

function showProgressBar(row: DocumentRowSummary): boolean {
  return normalizedProgress(row) !== null
}

function progressText(row: DocumentRowSummary): string | null {
  if (!showsProgressFeedback(row)) {
    return null
  }
  const progress = normalizedProgress(row)
  if (progress === null) {
    return null
  }
  return t('documents.workspace.rowState.progressLabel', { percent: progress })
}

function lastActivityText(row: DocumentRowSummary): string | null {
  if (!row.lastActivityAt || !showsProgressFeedback(row) || readinessKind(row) === 'graph_ready') {
    return null
  }
  return t('documents.workspace.rowState.lastActivity', {
    time: formatCompactDateTime(row.lastActivityAt),
  })
}
</script>

<template>
  <div class="rr-docs-table">
    <table>
      <colgroup>
        <col class="rr-docs-table__col rr-docs-table__col--name" />
        <col v-if="showTypeColumn" class="rr-docs-table__col rr-docs-table__col--type" />
        <col class="rr-docs-table__col rr-docs-table__col--size" />
        <col class="rr-docs-table__col rr-docs-table__col--date" />
        <col v-if="showCostColumn" class="rr-docs-table__col rr-docs-table__col--cost" />
        <col v-if="showStatusColumn" class="rr-docs-table__col rr-docs-table__col--status" />
      </colgroup>

      <thead>
        <tr class="rr-docs-table__head">
          <th
            class="rr-docs-table__th rr-docs-table__th--name"
            scope="col"
            :aria-sort="ariaSort('fileName')"
          >
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'fileName')"
            >
              <span>{{ $t('documents.workspace.table.name') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('fileName') }"
                aria-hidden="true"
              >
                {{ sortIndicator('fileName') }}
              </span>
            </button>
          </th>
          <th
            v-if="showTypeColumn"
            class="rr-docs-table__th rr-docs-table__th--type"
            scope="col"
            :aria-sort="ariaSort('fileType')"
          >
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'fileType')"
            >
              <span>{{ $t('documents.workspace.table.type') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('fileType') }"
                aria-hidden="true"
              >
                {{ sortIndicator('fileType') }}
              </span>
            </button>
          </th>
          <th
            class="rr-docs-table__th rr-docs-table__th--size"
            scope="col"
            :aria-sort="ariaSort('fileSizeBytes')"
          >
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'fileSizeBytes')"
            >
              <span>{{ $t('documents.workspace.table.size') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('fileSizeBytes') }"
                aria-hidden="true"
              >
                {{ sortIndicator('fileSizeBytes') }}
              </span>
            </button>
          </th>
          <th
            class="rr-docs-table__th rr-docs-table__th--date"
            scope="col"
            :aria-sort="ariaSort('uploadedAt')"
          >
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'uploadedAt')"
            >
              <span>{{ $t('documents.workspace.table.date') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('uploadedAt') }"
                aria-hidden="true"
              >
                {{ sortIndicator('uploadedAt') }}
              </span>
            </button>
          </th>
          <th
            v-if="showCostColumn"
            class="rr-docs-table__th rr-docs-table__th--cost"
            scope="col"
            :aria-sort="ariaSort('costAmount')"
          >
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'costAmount')"
            >
              <span>{{ $t('documents.workspace.table.cost') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('costAmount') }"
                aria-hidden="true"
              >
                {{ sortIndicator('costAmount') }}
              </span>
            </button>
          </th>
          <th
            v-if="showStatusColumn"
            class="rr-docs-table__th rr-docs-table__th--status"
            scope="col"
            :aria-sort="ariaSort('status')"
          >
            <button class="rr-docs-table__th-button" type="button" @click="emit('sort', 'status')">
              <span>{{ $t('documents.workspace.table.status') }}</span>
              <span
                class="rr-docs-table__sort-indicator"
                :class="{ 'is-active': isSortActive('status') }"
                aria-hidden="true"
              >
                {{ sortIndicator('status') }}
              </span>
            </button>
          </th>
        </tr>
      </thead>

      <tbody>
        <tr
          v-for="row in props.rows"
          :key="row.id"
          class="rr-docs-table__row"
          :class="{
            'is-selected': row.id === props.selectedId,
            'is-clickable': hasDetailTarget(row),
            'is-in-flight': isInFlight(row),
            'is-queued': isQueued(row),
            'is-processing': isProcessing(row),
            'is-readable': isReadable(row),
            'is-graph-sparse': isGraphSparse(row),
          }"
          :tabindex="hasDetailTarget(row) ? 0 : undefined"
          @click="openDetail(row)"
          @keydown.enter.prevent="openDetail(row)"
          @keydown.space.prevent="openDetail(row)"
        >
          <td class="rr-docs-table__cell rr-docs-table__cell--name" :title="row.fileName">
            <div class="rr-docs-table__name-stack">
              <div
                v-if="liveLabel(row)"
                class="rr-docs-table__live-indicator"
                :class="{
                  'is-queued': isQueued(row),
                  'is-processing': isProcessing(row),
                  'is-readable': isReadable(row),
                  'is-graph-sparse': isGraphSparse(row),
                }"
              >
                <span class="rr-docs-table__live-dot" aria-hidden="true" />
                <span>{{ liveLabel(row) }}</span>
              </div>
              <strong>{{ row.fileName }}</strong>
              <span>{{ compactMeta(row) }}</span>
            </div>
          </td>
          <td
            v-if="showTypeColumn"
            class="rr-docs-table__cell rr-docs-table__cell--type"
            :title="row.fileType"
          >
            {{ row.fileType }}
          </td>
          <td class="rr-docs-table__cell rr-docs-table__cell--size" :title="row.fileSizeLabel">
            {{ row.fileSizeLabel }}
          </td>
          <td
            class="rr-docs-table__cell rr-docs-table__cell--date"
            :title="formatDateTime(row.uploadedAt)"
          >
            {{ formatCompactDateTime(row.uploadedAt) }}
          </td>
          <td
            v-if="showCostColumn"
            class="rr-docs-table__cell rr-docs-table__cell--cost"
            :class="{ 'has-cost': row.costLabel }"
            :title="row.costLabel || '—'"
          >
            {{ row.costLabel || '—' }}
          </td>
          <td v-if="showStatusColumn" class="rr-docs-table__cell rr-docs-table__cell--status">
            <div
              class="rr-docs-table__status-stack"
              :class="{
                'is-in-flight': isInFlight(row),
                'is-readable': isReadable(row),
                'is-graph-sparse': isGraphSparse(row),
                'has-detail': hasStatusDetail(row),
              }"
            >
              <div class="rr-docs-table__status-top">
                <StatusPill :tone="statusTone(row)" :label="statusBadgeLabel(row)" />
                <button
                  v-if="row.canRetry"
                  class="rr-docs-table__status-action"
                  type="button"
                  @click.stop
                  @click="emit('retry', row.id)"
                >
                  {{ $t('documents.actions.retry') }}
                </button>
              </div>
              <p v-if="statusDetailText(row)" class="rr-docs-table__status-copy">
                {{ statusDetailText(row) }}
              </p>
              <div
                v-if="progressText(row) || lastActivityText(row)"
                class="rr-docs-table__status-meta"
              >
                <span v-if="progressText(row)">{{ progressText(row) }}</span>
                <span v-if="lastActivityText(row)">{{ lastActivityText(row) }}</span>
              </div>
              <div
                v-if="showProgressBar(row)"
                class="rr-docs-table__status-progress"
                :class="{
                  'is-queued': isQueued(row),
                  'is-processing': isProcessing(row),
                  'is-readable': isReadable(row),
                  'is-graph-sparse': isGraphSparse(row),
                }"
                aria-hidden="true"
              >
                <span :style="{ width: `${normalizedProgress(row) ?? 0}%` }" />
              </div>
            </div>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.rr-docs-table {
  position: relative;
  overflow-x: auto;
  overflow-y: hidden;
  border: 1px solid rgba(203, 213, 225, 0.86);
  border-top: 0;
  border-radius: 0 0 18px 18px;
  background: #fff;
  box-shadow:
    0 12px 24px rgba(15, 23, 42, 0.04),
    inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.rr-docs-table table {
  width: 100%;
  border-collapse: separate;
  border-spacing: 0;
  table-layout: fixed;
}

.rr-docs-table__col--name {
  width: auto;
}

.rr-docs-table__col--type {
  width: 70px;
}

.rr-docs-table__col--size {
  width: 78px;
}

.rr-docs-table__col--date {
  width: 118px;
}

.rr-docs-table__col--cost {
  width: 78px;
}

.rr-docs-table__col--status {
  width: 286px;
}

@media (min-width: 1500px) {
  .rr-docs-table__col--type {
    width: 82px;
  }

  .rr-docs-table__col--size {
    width: 90px;
  }

  .rr-docs-table__col--date {
    width: 138px;
  }

  .rr-docs-table__col--cost {
    width: 84px;
  }

  .rr-docs-table__col--status {
    width: 316px;
  }

  .rr-docs-table__th,
  .rr-docs-table__cell {
    padding-inline: 16px;
  }

  .rr-docs-table__cell {
    font-size: 0.86rem;
  }

  .rr-docs-table__name-stack strong {
    font-size: 0.88rem;
  }
}

@media (min-width: 1900px) {
  .rr-docs-table__col--type {
    width: 92px;
  }

  .rr-docs-table__col--size {
    width: 96px;
  }

  .rr-docs-table__col--date {
    width: 148px;
  }

  .rr-docs-table__col--cost {
    width: 88px;
  }

  .rr-docs-table__col--status {
    width: 336px;
  }

  .rr-docs-table__th,
  .rr-docs-table__cell {
    padding-inline: 18px;
  }

  .rr-docs-table__cell {
    padding-block: 10px;
    font-size: 0.87rem;
  }

  .rr-docs-table__name-stack strong {
    font-size: 0.95rem;
  }

  .rr-docs-table__name-stack span,
  .rr-docs-table__cell--type,
  .rr-docs-table__cell--size,
  .rr-docs-table__cell--date,
  .rr-docs-table__cell--cost {
    font-size: 0.8rem;
  }

  .rr-docs-table__th {
    padding-top: 11px;
    padding-bottom: 9px;
  }
}

.rr-docs-table__head {
  position: sticky;
  top: 0;
  z-index: 3;
  background: rgba(249, 250, 251, 0.96);
  box-shadow:
    inset 0 -1px 0 rgba(148, 163, 184, 0.92),
    inset 0 1px 0 rgba(255, 255, 255, 0.86);
}

.rr-docs-table__th {
  padding: 10px 14px 8px;
  font-size: 0.68rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.072em;
  color: color-mix(in srgb, var(--rr-text-secondary, #334155) 92%, #ffffff 8%);
  user-select: none;
  text-align: left;
  background: rgba(248, 250, 252, 0.96);
  backdrop-filter: blur(14px);
}

.rr-docs-table__th-button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 0;
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  letter-spacing: inherit;
  text-transform: inherit;
  cursor: pointer;
  transition: color 120ms ease;
}

.rr-docs-table__th-button:hover {
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-table__th-button:focus-visible {
  outline: 2px solid rgba(59, 130, 246, 0.28);
  outline-offset: 3px;
  border-radius: 6px;
}

.rr-docs-table__sort-indicator {
  opacity: 0.32;
  font-size: 0.74rem;
  line-height: 1;
}

.rr-docs-table__sort-indicator.is-active {
  opacity: 1;
  color: #1d4ed8;
}

.rr-docs-table__row {
  position: relative;
  outline: none;
  transition:
    background 120ms ease,
    box-shadow 120ms ease,
    transform 120ms ease;
}

.rr-docs-table__row + .rr-docs-table__row .rr-docs-table__cell {
  border-top: 1px solid rgba(226, 232, 240, 0.72);
}

.rr-docs-table__th--size,
.rr-docs-table__th--date,
.rr-docs-table__th--cost,
.rr-docs-table__cell--size,
.rr-docs-table__cell--date,
.rr-docs-table__cell--cost {
  text-align: right;
}

.rr-docs-table__th--size .rr-docs-table__th-button,
.rr-docs-table__th--date .rr-docs-table__th-button,
.rr-docs-table__th--cost .rr-docs-table__th-button {
  justify-content: flex-end;
}

.rr-docs-table__th--type,
.rr-docs-table__cell--type {
  text-align: center;
}

.rr-docs-table__th--type .rr-docs-table__th-button {
  justify-content: center;
}

.rr-docs-table__row.is-clickable {
  cursor: pointer;
}

.rr-docs-table__row.is-clickable:focus-visible {
  box-shadow: inset 0 0 0 2px rgba(59, 130, 246, 0.32);
}

.rr-docs-table__row.is-clickable:hover {
  background:
    linear-gradient(180deg, rgba(248, 250, 252, 0.92), rgba(255, 255, 255, 0.96)),
    rgba(248, 250, 252, 0.74);
}

.rr-docs-table__row.is-in-flight {
  box-shadow: inset 1px 0 0 rgba(59, 130, 246, 0.28);
}

.rr-docs-table__row.is-in-flight::after {
  content: '';
  position: absolute;
  inset: auto 0 0;
  height: 1px;
  opacity: 0.72;
}

.rr-docs-table__row.is-queued {
  background: rgba(255, 251, 235, 0.12);
  box-shadow: inset 1px 0 0 rgba(245, 158, 11, 0.34);
}

.rr-docs-table__row.is-queued::after {
  background: linear-gradient(90deg, rgba(245, 158, 11, 0.74), rgba(251, 191, 36, 0.28));
}

.rr-docs-table__row.is-processing {
  background: rgba(239, 246, 255, 0.14);
}

.rr-docs-table__row.is-processing::after {
  background: linear-gradient(90deg, rgba(37, 99, 235, 0.74), rgba(79, 70, 229, 0.28));
}

.rr-docs-table__row.is-readable,
.rr-docs-table__row.is-graph-sparse {
  background: rgba(240, 249, 255, 0.12);
  box-shadow: inset 1px 0 0 rgba(14, 116, 144, 0.28);
}

.rr-docs-table__row.is-readable::after,
.rr-docs-table__row.is-graph-sparse::after {
  background: linear-gradient(90deg, rgba(14, 116, 144, 0.68), rgba(56, 189, 248, 0.24));
}

.rr-docs-table__row.is-selected {
  background: rgba(238, 244, 255, 0.78);
  box-shadow:
    inset 1px 0 0 rgba(79, 70, 229, 0.7),
    inset 0 1px 0 rgba(255, 255, 255, 0.92);
}

.rr-docs-table__cell {
  padding: 7px 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.82rem;
  color: var(--rr-text-primary, #0f172a);
  vertical-align: top;
}

.rr-docs-table__cell--name {
  font-weight: 650;
}

.rr-docs-table__row.is-clickable:hover .rr-docs-table__cell--name strong {
  color: #1e40af;
}

.rr-docs-table__row.is-selected .rr-docs-table__cell--name strong {
  color: #1d4ed8;
}

.rr-docs-table__name-stack {
  display: grid;
  gap: 3px;
  min-width: 0;
}

.rr-docs-table__live-indicator {
  display: inline-flex;
  align-items: center;
  gap: 0.42rem;
  width: fit-content;
  min-height: 1rem;
  padding: 0 0.36rem;
  border-radius: 999px;
  border: 1px solid rgba(148, 163, 184, 0.22);
  background: rgba(255, 255, 255, 0.82);
  color: rgba(71, 85, 105, 0.96);
  font-size: 0.58rem;
  font-weight: 800;
  line-height: 1;
  letter-spacing: 0.02em;
  text-transform: uppercase;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.92);
}

.rr-docs-table__live-indicator.is-queued {
  border-color: rgba(245, 158, 11, 0.18);
  background: rgba(255, 251, 235, 0.92);
  color: rgba(180, 83, 9, 0.96);
}

.rr-docs-table__live-indicator.is-processing {
  border-color: rgba(96, 165, 250, 0.2);
  background: rgba(239, 246, 255, 0.94);
  color: rgba(29, 78, 216, 0.96);
}

.rr-docs-table__live-indicator.is-readable,
.rr-docs-table__live-indicator.is-graph-sparse {
  border-color: rgba(14, 116, 144, 0.18);
  background: rgba(240, 249, 255, 0.94);
  color: rgba(14, 116, 144, 0.96);
}

.rr-docs-table__live-dot {
  display: inline-flex;
  width: 0.44rem;
  height: 0.44rem;
  border-radius: 999px;
  background: currentColor;
  box-shadow: 0 0 0 0 rgba(37, 99, 235, 0.18);
  animation: rr-docs-table-live-pulse 1.8s ease-out infinite;
}

.rr-docs-table__name-stack strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.84rem;
  font-weight: 700;
  line-height: 1.35;
}

.rr-docs-table__name-stack span {
  display: none;
  color: var(--rr-text-secondary, rgba(15, 23, 42, 0.72));
  font-size: 0.75rem;
  line-height: 1.35;
}

.rr-docs-table__cell--type,
.rr-docs-table__cell--size,
.rr-docs-table__cell--date {
  color: var(--rr-text-secondary, rgba(15, 23, 42, 0.72));
  font-size: 0.75rem;
  font-variant-numeric: tabular-nums;
}

.rr-docs-table__cell--cost {
  color: color-mix(in srgb, var(--rr-text-secondary, #334155) 72%, #ffffff 28%);
  font-size: 0.73rem;
  font-variant-numeric: tabular-nums;
}

.rr-docs-table__cell--cost.has-cost {
  color: color-mix(in srgb, #4338ca 14%, var(--rr-text-secondary, #334155) 86%);
  font-weight: 500;
}

.rr-docs-table__cell--status {
  white-space: normal;
  padding-block: 8px;
}

.rr-docs-table__status-stack {
  display: grid;
  justify-items: start;
  gap: 0.28rem;
  min-width: 0;
  justify-self: stretch;
  width: 100%;
  overflow: hidden;
}

.rr-docs-table__status-top {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 6px;
  width: 100%;
}

.rr-docs-table__status-stack.is-in-flight {
  min-height: 0;
  padding: 0.36rem 0.46rem;
  border-radius: 10px;
  border: 1px solid rgba(191, 219, 254, 0.44);
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.82), rgba(246, 249, 255, 0.68)),
    rgba(255, 255, 255, 0.72);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.88);
}

.rr-docs-table__status-stack.is-readable,
.rr-docs-table__status-stack.is-graph-sparse {
  min-height: 0;
  padding: 0.36rem 0.46rem;
  border-radius: 10px;
  border: 1px solid rgba(186, 230, 253, 0.5);
  background:
    linear-gradient(180deg, rgba(248, 252, 255, 0.92), rgba(240, 249, 255, 0.74)),
    rgba(248, 252, 255, 0.78);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9);
}

.rr-docs-table__status-copy {
  margin: 0;
  color: rgba(71, 85, 105, 0.94);
  font-size: 0.65rem;
  font-weight: 600;
  line-height: 1.42;
  display: -webkit-box;
  overflow: hidden;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow-wrap: anywhere;
}

.rr-docs-table__status-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0.3rem 0.45rem;
  color: rgba(100, 116, 139, 0.92);
  font-size: 0.64rem;
  font-weight: 700;
  line-height: 1.35;
  width: 100%;
}

.rr-docs-table__status-meta span {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
}

.rr-docs-table__status-meta span + span::before {
  content: '•';
  color: rgba(148, 163, 184, 0.88);
  margin-right: 0.3rem;
}

.rr-docs-table__status-progress {
  position: relative;
  width: 100%;
  height: 0.26rem;
  overflow: hidden;
  border-radius: 999px;
  background: rgba(226, 232, 240, 0.9);
}

.rr-docs-table__status-progress span {
  position: absolute;
  inset: 0 auto 0 0;
  border-radius: inherit;
  transition: width 180ms ease;
}

.rr-docs-table__status-progress.is-queued span {
  background: linear-gradient(90deg, rgba(245, 158, 11, 0.88), rgba(251, 191, 36, 0.72));
}

.rr-docs-table__status-progress.is-processing span {
  background: linear-gradient(90deg, rgba(37, 99, 235, 0.9), rgba(79, 70, 229, 0.72));
}

.rr-docs-table__status-progress.is-readable span,
.rr-docs-table__status-progress.is-graph-sparse span {
  background: linear-gradient(90deg, rgba(14, 116, 144, 0.88), rgba(56, 189, 248, 0.62));
}

.rr-docs-table__status-stack :deep(.rr-status-pill) {
  background: rgba(241, 245, 249, 0.9);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.8);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--ready),
.rr-docs-table__status-stack :deep(.rr-status-pill--graph_ready) {
  background: rgba(240, 253, 248, 0.56);
  color: rgba(5, 150, 105, 0.78);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--readable),
.rr-docs-table__status-stack :deep(.rr-status-pill--graph_sparse) {
  background: rgba(240, 249, 255, 0.98);
  color: rgba(14, 116, 144, 0.96);
}

.rr-docs-table__status-stack.is-in-flight :deep(.rr-status-pill--queued) {
  background: rgba(255, 251, 235, 0.98);
  color: rgba(180, 83, 9, 0.96);
}

.rr-docs-table__status-stack.is-in-flight :deep(.rr-status-pill--processing) {
  background: rgba(239, 246, 255, 0.98);
  color: rgba(29, 78, 216, 0.96);
}

.rr-docs-table__status-action {
  display: inline-flex;
  align-items: center;
  flex-shrink: 0;
  margin-left: auto;
  padding: 2px 6px;
  border: 1px solid rgba(99, 102, 241, 0.16);
  border-radius: 999px;
  background: rgba(99, 102, 241, 0.06);
  font: inherit;
  font-size: 0.66rem;
  font-weight: 600;
  color: rgba(67, 56, 202, 0.92);
  cursor: pointer;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
  transition:
    background 100ms ease,
    border-color 100ms ease,
    color 100ms ease;
}

.rr-docs-table__status-action:hover {
  background: rgba(99, 102, 241, 0.1);
  border-color: rgba(99, 102, 241, 0.3);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--failed) {
  background: rgba(254, 242, 242, 0.82);
}

@keyframes rr-docs-table-live-pulse {
  0% {
    box-shadow: 0 0 0 0 rgba(37, 99, 235, 0.18);
  }

  70% {
    box-shadow: 0 0 0 0.36rem rgba(37, 99, 235, 0);
  }

  100% {
    box-shadow: 0 0 0 0 rgba(37, 99, 235, 0);
  }
}

@media (max-width: 1180px) {
  .rr-docs-table__col--status {
    width: 270px;
  }

  .rr-docs-table__cell--type,
  .rr-docs-table__cell--size,
  .rr-docs-table__cell--date,
  .rr-docs-table__cell--cost {
    font-size: 0.76rem;
  }
}

@media (max-width: 980px) {
  .rr-docs-table__col--type,
  .rr-docs-table__col--size,
  .rr-docs-table__th--type,
  .rr-docs-table__th--size,
  .rr-docs-table__cell--type,
  .rr-docs-table__cell--size {
    display: none;
  }

  .rr-docs-table__name-stack span {
    display: block;
  }
}

@media (max-width: 820px) {
  .rr-docs-table__col--date,
  .rr-docs-table__th--date,
  .rr-docs-table__cell--date {
    display: none;
  }
}

@media (max-width: 860px) {
  .rr-docs-table table,
  .rr-docs-table thead,
  .rr-docs-table tbody,
  .rr-docs-table tr,
  .rr-docs-table td {
    display: block;
    width: 100%;
  }

  .rr-docs-table tr.rr-docs-table__head {
    display: none;
  }

  .rr-docs-table tr.rr-docs-table__row {
    display: grid;
    grid-template-columns: 1fr;
    gap: 8px;
    padding: 10px 0;
  }

  .rr-docs-table td.rr-docs-table__cell--type,
  .rr-docs-table td.rr-docs-table__cell--size,
  .rr-docs-table td.rr-docs-table__cell--date,
  .rr-docs-table td.rr-docs-table__cell--cost {
    display: none;
  }

  .rr-docs-table td.rr-docs-table__cell {
    padding: 0 14px;
    border-bottom: 0;
  }

  .rr-docs-table__name-stack span {
    display: block;
  }

  .rr-docs-table td.rr-docs-table__cell--status {
    padding-top: 0;
    display: block;
  }

  .rr-docs-table__status-stack {
    justify-items: start;
    text-align: left;
    gap: 6px;
    padding: 0.52rem 0.62rem;
    width: 100%;
  }

  .rr-docs-table__status-copy {
    -webkit-line-clamp: 3;
  }

  .rr-docs-table__status-cost {
    display: inline-flex;
  }
}
</style>
