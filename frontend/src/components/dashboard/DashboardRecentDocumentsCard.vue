<script setup lang="ts">
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { computed } from 'vue'
import { i18n } from 'src/lib/i18n'
import type { DashboardRecentDocument } from 'src/models/ui/dashboard'

const props = defineProps<{
  documents: DashboardRecentDocument[]
  compact?: boolean
}>()

const t = (key: string) => i18n.global.t(key)
const { formatCompactDateTime } = useDisplayFormatters()
const showFileType = computed(() => new Set(props.documents.map((row) => row.fileType)).size > 1)
const showStatusBadge = computed(() => props.documents.some((row) => row.status !== 'ready'))
const useSoloLayout = computed(() => Boolean(props.compact) && props.documents.length === 1)

function statusKind(
  row: DashboardRecentDocument,
): 'queued' | 'processing' | 'failed' | 'graph_sparse' | 'graph_ready' {
  switch (row.status) {
    case 'queued':
    case 'processing':
    case 'failed':
      return row.status
    case 'ready_no_graph':
      return 'graph_sparse'
    case 'ready':
    default:
      return 'graph_ready'
  }
}

function statusLabel(row: DashboardRecentDocument): string {
  switch (row.status) {
    case 'queued':
      return t('documents.statuses.queued')
    case 'processing':
      return t('documents.statuses.processing')
    case 'failed':
      return t('documents.statuses.failed')
    case 'ready_no_graph':
      return t('documents.statuses.ready_no_graph')
    case 'ready':
    default:
      return t('documents.statuses.ready')
  }
}

function rowMeta(row: DashboardRecentDocument): string {
  return `${row.fileSizeLabel} · ${formatCompactDateTime(row.uploadedAt)}`
}
</script>

<template>
  <section class="rr-dash-docs" :class="{ 'is-compact': props.compact, 'is-solo': useSoloLayout }">
    <header class="rr-dash-docs__header">
      <div class="rr-dash-docs__copy">
        <div class="rr-dash-docs__title-row">
          <h2 class="rr-dash-docs__title">{{ t('dashboard.recent.title') }}</h2>
        </div>
        <p class="rr-dash-docs__subtitle">{{ t('dashboard.recent.subtitle') }}</p>
      </div>
    </header>

    <div v-if="props.documents.length" class="rr-dash-docs__table">
      <div v-for="row in props.documents" :key="row.id" class="rr-dash-docs__row">
        <div class="rr-dash-docs__name">
          <strong>{{ row.fileName }}</strong>
          <span class="rr-dash-docs__meta rr-dash-docs__meta--inline">{{ rowMeta(row) }}</span>
        </div>
        <span v-if="showFileType" class="rr-dash-docs__meta rr-dash-docs__meta--type">
          {{ row.fileType }}
        </span>
        <StatusBadge
          v-if="showStatusBadge"
          class="rr-dash-docs__status"
          :kind="statusKind(row)"
          :label="statusLabel(row)"
        />
      </div>
    </div>
    <p v-else class="rr-dash-docs__empty">
      {{ t('dashboard.recent.empty') }}
    </p>
  </section>
</template>
