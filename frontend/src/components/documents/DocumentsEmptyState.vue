<script setup lang="ts">
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'

defineProps<{
  loading: boolean
  hasDocuments?: boolean
  hasActiveFilters?: boolean
}>()
</script>

<template>
  <EmptyStateCard
    :title="
      loading
        ? $t('documents.loading')
        : hasDocuments && hasActiveFilters
          ? $t('documents.workspace.noMatchTitle')
          : $t('documents.workspace.emptyTitle')
    "
    :description="
      loading
        ? $t('documents.loading')
        : hasDocuments && hasActiveFilters
          ? $t('documents.workspace.noMatchDescription')
          : $t('documents.workspace.emptyDescription')
    "
  />
</template>
