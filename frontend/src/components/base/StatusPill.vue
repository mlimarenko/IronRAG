<script setup lang="ts">
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
  <span
    class="rr-status-pill"
    :class="`rr-status-pill--${props.tone}`"
  >{{ props.label }}</span>
</template>
