<script setup lang="ts">
import { computed } from 'vue'
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

const { formatCompactDateTime, formatDateTime } = useDisplayFormatters()
const showTypeColumn = computed(() => {
  const visibleTypes = new Set(props.rows.map((row) => row.fileType).filter((value) => value.trim().length > 0))
  return visibleTypes.size > 1
})
const showStatusColumn = computed(() => props.rows.some((row) => !['ready', 'ready_no_graph'].includes(row.status)))

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
  ].filter(Boolean).join(' · ')
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
</script>

<template>
  <div class="rr-docs-table">
    <table>
      <colgroup>
        <col class="rr-docs-table__col rr-docs-table__col--name">
        <col
          v-if="showTypeColumn"
          class="rr-docs-table__col rr-docs-table__col--type"
        >
        <col class="rr-docs-table__col rr-docs-table__col--size">
        <col class="rr-docs-table__col rr-docs-table__col--date">
        <col class="rr-docs-table__col rr-docs-table__col--cost">
        <col
          v-if="showStatusColumn"
          class="rr-docs-table__col rr-docs-table__col--status"
        >
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
            <button
              class="rr-docs-table__th-button"
              type="button"
              @click="emit('sort', 'status')"
            >
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
          }"
          :tabindex="hasDetailTarget(row) ? 0 : undefined"
          @click="openDetail(row)"
          @keydown.enter.prevent="openDetail(row)"
          @keydown.space.prevent="openDetail(row)"
        >
          <td class="rr-docs-table__cell rr-docs-table__cell--name" :title="row.fileName">
            <div class="rr-docs-table__name-stack">
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
          <td
            class="rr-docs-table__cell rr-docs-table__cell--size"
            :title="row.fileSizeLabel"
          >
            {{ row.fileSizeLabel }}
          </td>
          <td
            class="rr-docs-table__cell rr-docs-table__cell--date"
            :title="formatDateTime(row.uploadedAt)"
          >
            {{ formatCompactDateTime(row.uploadedAt) }}
          </td>
          <td
            class="rr-docs-table__cell rr-docs-table__cell--cost"
            :class="{ 'has-cost': row.costLabel }"
            :title="row.costLabel || '—'"
          >
            {{ row.costLabel || '—' }}
          </td>
          <td
            v-if="showStatusColumn"
            class="rr-docs-table__cell rr-docs-table__cell--status"
          >
            <div class="rr-docs-table__status-stack">
              <StatusPill
                :tone="row.status"
                :label="row.stageLabel && (row.status === 'processing' || row.status === 'queued') ? row.stageLabel : row.statusLabel"
              />
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
  overflow-y: visible;
  border: 1px solid rgba(203, 213, 225, 0.86);
  border-top: 0;
  border-radius: 0 0 16px 16px;
  background: #fff;
  box-shadow:
    0 18px 30px rgba(15, 23, 42, 0.05),
    inset 0 1px 0 rgba(255, 255, 255, 0.7);
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
  width: 76px;
}

.rr-docs-table__col--size {
  width: 84px;
}

.rr-docs-table__col--date {
  width: 132px;
}

.rr-docs-table__col--cost {
  width: 90px;
}

.rr-docs-table__col--status {
  width: 136px;
}

@media (min-width: 1500px) {
  .rr-docs-table__col--type {
    width: 92px;
  }

  .rr-docs-table__col--size {
    width: 100px;
  }

  .rr-docs-table__col--date {
    width: 156px;
  }

  .rr-docs-table__col--cost {
    width: 104px;
  }

  .rr-docs-table__col--status {
    width: 152px;
  }

  .rr-docs-table__th,
  .rr-docs-table__cell {
    padding-inline: 18px;
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
    width: 104px;
  }

  .rr-docs-table__col--size {
    width: 112px;
  }

  .rr-docs-table__col--date {
    width: 168px;
  }

  .rr-docs-table__col--cost {
    width: 112px;
  }

  .rr-docs-table__col--status {
    width: 164px;
  }

  .rr-docs-table__th,
  .rr-docs-table__cell {
    padding-inline: 22px;
  }

  .rr-docs-table__cell {
    padding-block: 13px;
    font-size: 0.89rem;
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
  background: rgba(248, 250, 252, 0.96);
  box-shadow:
    inset 0 -1px 0 rgba(148, 163, 184, 0.92),
    inset 0 1px 0 rgba(255, 255, 255, 0.86);
}

.rr-docs-table__th {
  padding: 12px 16px 10px;
  font-size: 0.72rem;
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
  outline: none;
  transition: background 120ms ease, box-shadow 120ms ease, transform 120ms ease;
}

.rr-docs-table__row + .rr-docs-table__row .rr-docs-table__cell {
  border-top: 1px solid rgba(226, 232, 240, 0.72);
}

.rr-docs-table__th--size,
.rr-docs-table__th--date,
.rr-docs-table__th--cost,
.rr-docs-table__th--status,
.rr-docs-table__cell--size,
.rr-docs-table__cell--date,
.rr-docs-table__cell--cost,
.rr-docs-table__cell--status {
  text-align: right;
}

.rr-docs-table__th--size .rr-docs-table__th-button,
.rr-docs-table__th--date .rr-docs-table__th-button,
.rr-docs-table__th--cost .rr-docs-table__th-button,
.rr-docs-table__th--status .rr-docs-table__th-button {
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
  background: linear-gradient(90deg, rgba(242, 246, 255, 0.98), rgba(247, 249, 255, 0.92));
}

.rr-docs-table__row.is-selected {
  background: linear-gradient(90deg, rgba(232, 240, 255, 0.98), rgba(241, 246, 255, 0.92));
  box-shadow:
    inset 4px 0 0 rgba(79, 70, 229, 0.9),
    inset 0 1px 0 rgba(255, 255, 255, 0.92);
}

.rr-docs-table__cell {
  padding: 12px 16px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.84rem;
  color: var(--rr-text-primary, #0f172a);
  vertical-align: middle;
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
  gap: 4px;
  min-width: 0;
}

.rr-docs-table__name-stack strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.89rem;
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
  font-size: 0.79rem;
  font-variant-numeric: tabular-nums;
}

.rr-docs-table__cell--cost {
  color: color-mix(in srgb, var(--rr-text-secondary, #334155) 72%, #ffffff 28%);
  font-size: 0.77rem;
  font-variant-numeric: tabular-nums;
}

.rr-docs-table__cell--cost.has-cost {
  color: color-mix(in srgb, #4338ca 14%, var(--rr-text-secondary, #334155) 86%);
  font-weight: 500;
}

.rr-docs-table__cell--status {
  white-space: normal;
}

.rr-docs-table__status-stack {
  display: grid;
  justify-items: end;
  gap: 5px;
}

.rr-docs-table__status-stack :deep(.rr-status-pill) {
  background: rgba(241, 245, 249, 0.9);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.8);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--ready) {
  background: rgba(240, 253, 248, 0.56);
  color: rgba(5, 150, 105, 0.78);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--ready_no_graph) {
  background: rgba(248, 250, 252, 0.92);
  color: rgba(71, 85, 105, 0.92);
}

.rr-docs-table__status-action {
  display: inline-flex;
  align-items: center;
  padding: 2px 8px;
  border: 1px solid rgba(99, 102, 241, 0.16);
  border-radius: 999px;
  background: rgba(99, 102, 241, 0.06);
  font: inherit;
  font-size: 0.7rem;
  font-weight: 600;
  color: rgba(67, 56, 202, 0.92);
  cursor: pointer;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
  transition: background 100ms ease, border-color 100ms ease, color 100ms ease;
}

.rr-docs-table__status-action:hover {
  background: rgba(99, 102, 241, 0.1);
  border-color: rgba(99, 102, 241, 0.3);
}

.rr-docs-table__status-stack :deep(.rr-status-pill--failed) {
  background: rgba(254, 242, 242, 0.82);
}

@media (max-width: 1180px) {
  .rr-docs-table__name-stack {
    gap: 3px;
  }

  .rr-docs-table__name-stack span {
    display: block;
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
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 0;
    padding: 10px 0;
  }

  .rr-docs-table td.rr-docs-table__cell--type,
  .rr-docs-table td.rr-docs-table__cell--size,
  .rr-docs-table td.rr-docs-table__cell--date,
  .rr-docs-table td.rr-docs-table__cell--cost {
    display: none;
  }

  .rr-docs-table td.rr-docs-table__cell {
    padding: 2px 16px;
    border-bottom: 0;
  }

  .rr-docs-table__name-stack span {
    display: block;
  }

  .rr-docs-table td.rr-docs-table__cell--status {
    padding-top: 6px;
    display: flex;
    justify-content: flex-end;
    align-items: flex-start;
  }

  .rr-docs-table__status-stack {
    justify-items: end;
    text-align: right;
    gap: 8px;
  }

  .rr-docs-table__status-cost {
    display: inline-flex;
  }
}
</style>
