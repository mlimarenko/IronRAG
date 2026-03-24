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
}>()

const emit = defineEmits<{
  updateSearch: [value: string]
  updateStatus: [value: DocumentDisplayStatus | '']
}>()

const hasActiveFilter = computed(() => props.searchQuery.trim().length > 0 || props.statusFilter !== '')
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

    <template #summary>
      <p
        v-if="metaLabel"
        class="rr-documents-filters__caption"
      >
        {{ $t(metaLabel.key, metaLabel.params) }}
      </p>
      <p
        v-else-if="hasActiveFilter"
        class="rr-documents-filters__caption"
      >
        {{ $t('documents.workspace.filtersApplied', { count: activeFilterCount }) }}
      </p>
    </template>
  </FilterBar>
</template>

<style scoped lang="scss">
.rr-documents-filters {
  display: grid;
  gap: 0.6rem;
  padding: 0.35rem 0.35rem 0;
}

.rr-documents-filters__caption {
  margin: 0;
  color: var(--rr-text-muted);
  font-size: 0.78rem;
  line-height: 1.35;
}

@media (max-width: 720px) {
  .rr-documents-filters {
    padding-inline: 0;
  }
}
</style>
