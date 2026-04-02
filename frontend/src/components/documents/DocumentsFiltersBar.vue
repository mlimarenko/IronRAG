<script setup lang="ts">
import { computed } from 'vue'
import FilterBar from 'src/components/design-system/FilterBar.vue'
import SearchField from 'src/components/design-system/SearchField.vue'
import SelectField from 'src/components/design-system/SelectField.vue'
import WebIngestRunActivityStrip from './WebIngestRunActivityStrip.vue'
import type { DocumentDisplayStatus, WebIngestRunSummary } from 'src/models/ui/documents'

const props = defineProps<{
  searchQuery: string
  statusFilter: DocumentDisplayStatus | ''
  visibleCount?: number
  totalCount?: number
  showMeta?: boolean
  activeProcessingCount?: number
  activeReadableCount?: number
  activeGraphSparseCount?: number
  activeWebRuns?: WebIngestRunSummary[]
  recentWebRuns?: WebIngestRunSummary[]
  webRunActionRunId?: string | null
}>()

const emit = defineEmits<{
  updateSearch: [value: string]
  updateStatus: [value: DocumentDisplayStatus | '']
  openWebRun: [runId: string]
  cancelWebRun: [runId: string]
}>()

const hasActiveFilter = computed(
  () => props.searchQuery.trim().length > 0 || props.statusFilter !== '',
)
const activeFilterCount = computed(
  () => Number(Boolean(props.searchQuery.trim())) + Number(props.statusFilter !== ''),
)

const metaLabel = computed(() => {
  if (typeof props.visibleCount !== 'number' || typeof props.totalCount !== 'number') {
    return null
  }
  if (!props.showMeta || props.visibleCount === props.totalCount) {
    return null
  }
  return {
    key: 'documents.workspace.filteredDocuments',
    params: {
      visible: props.visibleCount,
      total: props.totalCount,
    },
  }
})

const showSummary = computed(() => Boolean(metaLabel.value) || hasActiveFilter.value)

const visibleCount = computed(() => {
  if (typeof props.visibleCount === 'number') {
    return props.visibleCount
  }
  if (typeof props.totalCount === 'number') {
    return props.totalCount
  }
  return 0
})

const activeProcessingCount = computed(() => props.activeProcessingCount ?? 0)
const activeReadableCount = computed(() => props.activeReadableCount ?? 0)
const activeGraphSparseCount = computed(() => props.activeGraphSparseCount ?? 0)
const activeBacklogCount = computed(() => activeProcessingCount.value)
const showActivityStrip = computed(
  () =>
    activeBacklogCount.value > 0 ||
    activeReadableCount.value > 0 ||
    activeGraphSparseCount.value > 0,
)
const showWebRunStrip = computed(
  () => (props.activeWebRuns?.length ?? 0) > 0 || (props.recentWebRuns?.length ?? 0) > 0,
)
</script>

<template>
  <FilterBar class="rr-documents-filters">
    <template #search>
      <SearchField
        :model-value="props.searchQuery"
        :placeholder="$t('documents.search')"
        @update:model-value="emit('updateSearch', $event)"
        @clear="emit('updateSearch', '')"
      />
    </template>

    <template #filters>
      <SelectField
        :model-value="props.statusFilter"
        :options="[
          { id: '', label: $t('documents.allStatuses') },
          { id: 'in_progress', label: $t('documents.displayStatus.in_progress') },
          { id: 'ready', label: $t('documents.displayStatus.ready') },
          { id: 'failed', label: $t('documents.displayStatus.failed') },
        ]"
        @update:model-value="emit('updateStatus', $event as DocumentDisplayStatus | '')"
      />
    </template>

    <template v-if="showSummary" #summary>
      <div class="rr-documents-filters__summary">
        <span class="rr-documents-filters__count">{{ visibleCount }}</span>
        <p v-if="metaLabel" class="rr-documents-filters__caption">
          {{ $t(metaLabel.key, metaLabel.params) }}
        </p>
        <p v-else-if="hasActiveFilter" class="rr-documents-filters__caption">
          {{ $t('documents.workspace.filtersApplied', { count: activeFilterCount }) }}
        </p>
      </div>
    </template>
  </FilterBar>

  <div
    v-if="showActivityStrip"
    class="rr-documents-filters__activity"
    role="status"
    aria-live="polite"
  >
    <div class="rr-documents-filters__activity-main">
      <span class="rr-documents-filters__activity-pulse" aria-hidden="true" />
      <strong v-if="activeBacklogCount > 0">
        {{ $t('documents.workspace.processingStrip.title', { count: activeBacklogCount }) }}
      </strong>
      <strong v-else>
        {{
          $t('documents.workspace.processingStrip.readinessTitle', {
            readable: activeReadableCount,
            graphSparse: activeGraphSparseCount,
          })
        }}
      </strong>
    </div>

    <div class="rr-documents-filters__activity-breakdown">
      <span v-if="activeProcessingCount > 0">
        {{ $t('documents.workspace.processingStrip.processing', { count: activeProcessingCount }) }}
      </span>
      <span v-if="activeReadableCount > 0">
        {{ $t('documents.workspace.processingStrip.readable', { count: activeReadableCount }) }}
      </span>
      <span v-if="activeGraphSparseCount > 0" class="is-graph-sparse">
        {{
          $t('documents.workspace.processingStrip.graphSparse', { count: activeGraphSparseCount })
        }}
      </span>
    </div>
  </div>

  <WebIngestRunActivityStrip
    v-if="showWebRunStrip"
    :active-runs="props.activeWebRuns ?? []"
    :recent-runs="props.recentWebRuns ?? []"
    :canceling-run-id="props.webRunActionRunId"
    @open-run="emit('openWebRun', $event)"
    @cancel-run="emit('cancelWebRun', $event)"
  />
</template>

<style scoped lang="scss">
.rr-documents-filters {
  position: sticky;
  top: var(--rr-docs-sticky-top, 4.85rem);
  z-index: 8;
  display: grid;
  align-content: start;
  gap: 0.16rem;
  margin: 0;
  padding: 0.16rem 0.18rem 0.12rem;
  border: 1px solid rgba(226, 232, 240, 0.88);
  border-radius: 12px 12px 0 0;
  border-bottom-color: rgba(203, 213, 225, 0.9);
  background: rgba(255, 255, 255, 0.96);
  backdrop-filter: blur(10px);
  box-shadow: 0 6px 14px rgba(15, 23, 42, 0.025);
}

.rr-documents-filters :deep(.rr-filter-bar) {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 0.24rem 0.55rem;
  align-items: center;
}

.rr-documents-filters :deep(.rr-filter-bar__controls) {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 148px;
  gap: 6px;
}

.rr-documents-filters :deep(.rr-filter-bar__search),
.rr-documents-filters :deep(.rr-filter-bar__filters) {
  align-items: stretch;
}

.rr-documents-filters :deep(.rr-search-field),
.rr-documents-filters :deep(.rr-field--select) {
  height: 100%;
}

.rr-documents-filters :deep(.rr-field) {
  min-height: 2.42rem;
  border-color: rgba(203, 213, 225, 0.88);
  background: rgba(255, 255, 255, 0.94);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.88);
}

.rr-documents-filters :deep(.rr-field:hover) {
  border-color: rgba(99, 102, 241, 0.22);
}

.rr-documents-filters :deep(.rr-filter-bar__meta) {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  min-height: 1.35rem;
  white-space: nowrap;
}

.rr-documents-filters__summary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-height: 1.35rem;
}

.rr-documents-filters__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.65rem;
  height: 1.3rem;
  padding: 0 0.38rem;
  border-radius: 999px;
  background: rgba(79, 70, 229, 0.08);
  color: rgba(67, 56, 202, 0.96);
  font-size: 0.68rem;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}

.rr-documents-filters__caption {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.72rem;
  font-weight: 500;
  line-height: 1.35;
}

.rr-documents-filters__activity {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.42rem 0.72rem;
  margin-top: 0.06rem;
  padding: 0.28rem 0.42rem;
  border: 1px solid rgba(191, 219, 254, 0.65);
  border-radius: 10px;
  background: rgba(248, 250, 252, 0.7);
}

.rr-documents-filters__activity-main {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
}

.rr-documents-filters__activity-main strong {
  color: rgba(30, 64, 175, 0.96);
  font-size: 0.72rem;
  font-weight: 700;
  line-height: 1.35;
}

.rr-documents-filters__activity-pulse {
  display: inline-flex;
  width: 0.56rem;
  height: 0.56rem;
  border-radius: 999px;
  background: linear-gradient(135deg, #2563eb, #4f46e5);
  box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.2);
  animation: rr-documents-filters-pulse 1.8s ease-out infinite;
}

.rr-documents-filters__activity-breakdown {
  display: inline-flex;
  flex-wrap: wrap;
  gap: 0.42rem;
}

.rr-documents-filters__activity-breakdown span {
  display: inline-flex;
  align-items: center;
  min-height: 1.52rem;
  padding: 0 0.52rem;
  border-radius: 999px;
  border: 1px solid rgba(203, 213, 225, 0.9);
  background: rgba(255, 255, 255, 0.9);
  color: rgba(71, 85, 105, 0.9);
  font-size: 0.67rem;
  font-weight: 700;
  line-height: 1;
}

.rr-documents-filters__activity-breakdown span.is-graph-sparse {
  border-color: rgba(14, 116, 144, 0.18);
  background: rgba(240, 249, 255, 0.92);
  color: rgba(14, 116, 144, 0.96);
}

@keyframes rr-documents-filters-pulse {
  0% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.2);
  }

  70% {
    box-shadow: 0 0 0 0.38rem rgba(79, 70, 229, 0);
  }

  100% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0);
  }
}

@media (min-width: 1800px) {
  .rr-documents-filters {
    padding-inline: 0.5rem;
  }
}

@media (max-width: 920px) {
  .rr-documents-filters {
    padding: 0.22rem 0.24rem 0.2rem;
    border-radius: 12px 12px 0 0;
  }

  .rr-documents-filters__activity {
    padding: 0.42rem 0.52rem;
  }
}

@media (max-width: 720px) {
  .rr-documents-filters :deep(.rr-filter-bar__meta) {
    align-items: flex-start;
    flex-direction: column;
  }

  .rr-documents-filters__activity {
    align-items: flex-start;
    justify-content: flex-start;
  }
}

@media (max-width: 560px) {
  .rr-documents-filters :deep(.rr-filter-bar) {
    grid-template-columns: 1fr;
    gap: 7px;
  }

  .rr-documents-filters :deep(.rr-filter-bar__controls) {
    display: grid;
    grid-template-columns: 1fr;
    gap: 7px;
  }

  .rr-documents-filters :deep(.rr-filter-bar__controls > .rr-search-field),
  .rr-documents-filters :deep(.rr-filter-bar__controls > .rr-field--select) {
    flex: none;
    width: 100%;
    min-width: 0;
  }

  .rr-documents-filters :deep(.rr-filter-bar__meta) {
    width: 100%;
  }

  .rr-documents-filters__summary {
    width: 100%;
    justify-content: flex-start;
  }

  .rr-documents-filters__activity-main {
    align-items: flex-start;
  }
}
</style>
