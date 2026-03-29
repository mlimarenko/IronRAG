<script setup lang="ts">
import FeedbackState from 'src/components/design-system/FeedbackState.vue'

const props = defineProps<{
  loading: boolean
  hasDocuments?: boolean
  hasActiveFilters?: boolean
}>()

const emit = defineEmits<{
  upload: []
  clearFilters: []
}>()
</script>

<template>
  <FeedbackState
    v-if="props.loading"
    kind="loading"
    :title="$t('documents.loading')"
    :message="$t('documents.workspace.loadingDescription')"
  />

  <FeedbackState
    v-else-if="!props.hasDocuments && !props.hasActiveFilters"
    kind="empty"
    :title="$t('documents.workspace.emptyTitle')"
    :message="$t('documents.workspace.emptyDescription')"
    :action-label="$t('documents.actions.upload')"
    @action="emit('upload')"
  />

  <FeedbackState
    v-else
    kind="sparse"
    :title="$t('documents.workspace.noMatchTitle')"
    :message="$t('documents.workspace.noMatchDescription')"
    :action-label="$t('documents.actions.clearFilters')"
    @action="emit('clearFilters')"
  />
</template>
