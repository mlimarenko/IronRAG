<script setup lang="ts">
const props = defineProps<{
  kind:
    | 'ready'
    | 'warning'
    | 'failed'
    | 'partial'
    | 'rebuilding'
    | 'disabled'
    | 'info'
    | 'queued'
    | 'processing'
    | 'active'
    | 'blocked'
    | 'retrying'
    | 'stalled'
    | 'ready_no_graph'
  label: string
}>()
</script>

<template>
  <span
    class="rr-status-pill"
    :class="`rr-status-pill--${props.kind}`"
  >
    {{ props.label }}
  </span>
</template>
