<script setup lang="ts">
import FeedbackState from 'src/components/design-system/FeedbackState.vue'

defineProps<{
  title: string
  description: string
}>()
</script>

<template>
  <FeedbackState
    :title="title"
    :message="description"
    kind="empty"
  />
</template>
