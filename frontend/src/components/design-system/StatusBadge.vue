<script setup lang="ts">
const props = defineProps<{
  kind:
    | 'ready'
    | 'graph_ready'
    | 'readable'
    | 'graph_sparse'
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
  label: string
}>()
</script>

<template>
  <span class="rr-status-pill" :class="`rr-status-pill--${props.kind}`">
    {{ props.label }}
  </span>
</template>
