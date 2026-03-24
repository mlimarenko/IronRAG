<script setup lang="ts">
import StatusBadge from 'src/components/design-system/StatusBadge.vue'

const props = defineProps<{
  tone:
    | 'queued'
    | 'processing'
    | 'ready'
    | 'ready_no_graph'
    | 'failed'
    | 'active'
    | 'blocked'
    | 'retrying'
    | 'stalled'
  label: string
}>()
</script>

<template>
  <StatusBadge
    :kind="props.tone"
    :label="props.label"
  />
</template>
