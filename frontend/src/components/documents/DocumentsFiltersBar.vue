<script setup lang="ts">
import { computed } from 'vue'
import FilterBar from 'src/components/design-system/FilterBar.vue'
import SearchField from 'src/components/design-system/SearchField.vue'
import SelectField from 'src/components/design-system/SelectField.vue'
import type { DocumentDisplayStatus } from 'src/models/ui/documents'

const props = defineProps<{
  searchQuery: string
  statusFilter: DocumentDisplayStatus | ''
  visibleCount?: number
  totalCount?: number
  showMeta?: boolean
  activeQueuedCount?: number
  activeProcessingCount?: number
  activeReadyNoGraphCount?: number
}>()

const emit = defineEmits<{
  updateSearch: [value: string]
  updateStatus: [value: DocumentDisplayStatus | '']
}>()

const hasActiveFilter = computed(
  () => props.searchQuery.trim().length > 0 || props.statusFilter !== '',
)
const activeFilterCount = computed(
  () => Number(Boolean(props.searchQuery.trim())) + Number(props.statusFilter !== ''),
)

const metaLabel = computed(() => {
  if (
    !props.showMeta ||
    typeof props.visibleCount !== 'number' ||
    typeof props.totalCount !== 'number' ||
    props.visibleCount === props.totalCount
  ) {
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

const activeQueuedCount = computed(() => props.activeQueuedCount ?? 0)
const activeProcessingCount = computed(() => props.activeProcessingCount ?? 0)
const activeReadyNoGraphCount = computed(() => props.activeReadyNoGraphCount ?? 0)
const activeBacklogCount = computed(() => activeQueuedCount.value + activeProcessingCount.value)
const showActivityStrip = computed(
  () => activeBacklogCount.value > 0 || activeReadyNoGraphCount.value > 0,
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
        {{ $t('documents.workspace.processingStrip.graphCatchUpTitle', { count: activeReadyNoGraphCount }) }}
      </strong>
    </div>

    <div class="rr-documents-filters__activity-breakdown">
      <span v-if="activeProcessingCount > 0">
        {{ $t('documents.workspace.processingStrip.processing', { count: activeProcessingCount }) }}
      </span>
      <span v-if="activeQueuedCount > 0">
        {{ $t('documents.workspace.processingStrip.queued', { count: activeQueuedCount }) }}
      </span>
      <span v-if="activeReadyNoGraphCount > 0" class="is-graph-catchup">
        {{ $t('documents.workspace.processingStrip.graphCatchUp', { count: activeReadyNoGraphCount }) }}
      </span>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-documents-filters {
  position: sticky;
  top: var(--rr-docs-sticky-top, 4.85rem);
  z-index: 8;
  display: grid;
  gap: 0.42rem;
  margin: -0.1rem -0.1rem 0;
  padding: 0.42rem 0.46rem 0.34rem;
  border: 1px solid rgba(203, 213, 225, 0.8);
  border-radius: 18px 18px 0 0;
  border-bottom-color: rgba(203, 213, 225, 0.88);
  background: linear-gradient(
    180deg,
    rgba(255, 255, 255, 0.985),
    rgba(248, 250, 252, 0.965) 78%,
    rgba(255, 255, 255, 0.88)
  );
  backdrop-filter: blur(16px);
  box-shadow:
    0 10px 18px rgba(15, 23, 42, 0.04),
    inset 0 1px 0 rgba(255, 255, 255, 0.9);
}

.rr-documents-filters :deep(.rr-filter-bar) {
  display: grid;
  gap: 0.4rem;
}

.rr-documents-filters :deep(.rr-filter-bar__controls) {
  gap: 8px;
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
  min-height: 2.62rem;
  border-color: rgba(148, 163, 184, 0.34);
  background: rgba(255, 255, 255, 0.92);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
}

.rr-documents-filters :deep(.rr-field:hover) {
  border-color: rgba(99, 102, 241, 0.24);
}

.rr-documents-filters :deep(.rr-filter-bar__meta) {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  min-height: 1.35rem;
}

.rr-documents-filters__summary {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-height: 1.35rem;
}

.rr-documents-filters__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 2.05rem;
  height: 1.58rem;
  padding: 0 0.48rem;
  border-radius: 999px;
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.14), rgba(59, 130, 246, 0.1));
  color: rgba(55, 48, 163, 0.96);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.01em;
  font-variant-numeric: tabular-nums;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.58);
}

.rr-documents-filters__caption {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.79rem;
  font-weight: 500;
  line-height: 1.35;
}

.rr-documents-filters__activity {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 0.7rem 1rem;
  margin-top: 0.42rem;
  padding: 0.72rem 0.92rem;
  border: 1px solid rgba(96, 165, 250, 0.22);
  border-radius: 14px;
  background:
    linear-gradient(135deg, rgba(239, 246, 255, 0.96), rgba(245, 248, 255, 0.92)),
    rgba(255, 255, 255, 0.94);
  box-shadow:
    0 10px 22px rgba(37, 99, 235, 0.08),
    inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.rr-documents-filters__activity-main {
  display: inline-flex;
  align-items: center;
  gap: 0.56rem;
  min-width: 0;
}

.rr-documents-filters__activity-main strong {
  color: rgba(30, 64, 175, 0.96);
  font-size: 0.83rem;
  font-weight: 800;
  letter-spacing: -0.01em;
  line-height: 1.35;
}

.rr-documents-filters__activity-pulse {
  position: relative;
  display: inline-flex;
  width: 0.68rem;
  height: 0.68rem;
  border-radius: 999px;
  background: linear-gradient(135deg, #2563eb, #4f46e5);
  box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.28);
  animation: rr-documents-filters-pulse 1.8s ease-out infinite;
}

.rr-documents-filters__activity-breakdown {
  display: inline-flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 0.45rem;
}

.rr-documents-filters__activity-breakdown span {
  display: inline-flex;
  align-items: center;
  min-height: 1.85rem;
  padding: 0 0.7rem;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.88);
  border: 1px solid rgba(148, 163, 184, 0.22);
  color: rgba(71, 85, 105, 0.92);
  font-size: 0.74rem;
  font-weight: 700;
  line-height: 1;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.84);
}

.rr-documents-filters__activity-breakdown span.is-graph-catchup {
  border-color: rgba(14, 116, 144, 0.18);
  background: rgba(240, 249, 255, 0.92);
  color: rgba(14, 116, 144, 0.96);
}

@keyframes rr-documents-filters-pulse {
  0% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.24);
  }

  70% {
    box-shadow: 0 0 0 0.48rem rgba(79, 70, 229, 0);
  }

  100% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0);
  }
}

@media (max-width: 920px) {
  .rr-documents-filters {
    gap: 0.38rem;
    padding: 0.34rem 0.34rem 0.3rem;
    border-radius: 16px 16px 0 0;
    backdrop-filter: blur(12px);
    box-shadow:
      0 8px 16px rgba(15, 23, 42, 0.038),
      inset 0 1px 0 rgba(255, 255, 255, 0.88);
  }

  .rr-documents-filters :deep(.rr-filter-bar) {
    gap: 0.36rem;
  }

  .rr-documents-filters :deep(.rr-filter-bar__controls) {
    gap: 8px;
  }

  .rr-documents-filters :deep(.rr-field) {
    min-height: 2.45rem;
  }

  .rr-documents-filters :deep(.rr-filter-bar__meta) {
    gap: 7px;
  }

  .rr-documents-filters__summary {
    gap: 7px;
    min-height: 1.35rem;
  }

  .rr-documents-filters__count {
    min-width: 2rem;
    height: 1.55rem;
    padding-inline: 0.48rem;
    font-size: 0.71rem;
  }

  .rr-documents-filters__caption {
    font-size: 0.74rem;
  }

  .rr-documents-filters__activity {
    gap: 0.6rem 0.8rem;
    padding: 0.68rem 0.78rem;
  }

  .rr-documents-filters__activity-main strong {
    font-size: 0.79rem;
  }

  .rr-documents-filters__activity-breakdown span {
    min-height: 1.76rem;
    padding-inline: 0.64rem;
    font-size: 0.72rem;
  }
}

@media (min-width: 1800px) {
  .rr-documents-filters {
    padding-inline: 0.65rem;
  }

  .rr-documents-filters :deep(.rr-filter-bar__controls) {
    gap: 12px;
  }

  .rr-documents-filters :deep(.rr-field) {
    min-height: 2.85rem;
  }
}

@media (max-width: 720px) {
  .rr-documents-filters {
    padding-inline: 0;
  }

  .rr-documents-filters :deep(.rr-filter-bar__meta) {
    align-items: flex-start;
    flex-direction: column;
  }

  .rr-documents-filters__activity {
    align-items: flex-start;
    justify-content: flex-start;
  }

  .rr-documents-filters__activity-breakdown {
    justify-content: flex-start;
  }
}

@media (max-width: 560px) {
  .rr-documents-filters :deep(.rr-filter-bar) {
    display: grid;
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

  .rr-documents-filters :deep(.rr-field) {
    min-height: 2.38rem;
  }

  .rr-documents-filters :deep(.rr-filter-bar__meta) {
    width: 100%;
  }

  .rr-documents-filters__summary {
    width: 100%;
    justify-content: flex-start;
  }
}
</style>
