<script setup lang="ts">
import { computed } from 'vue'

import { formatStatusLabel, getStatusTone } from './statusFormatting'

const props = defineProps<{
  status?: string
}>()

const tone = computed(() => getStatusTone(props.status))
const label = computed(() => formatStatusLabel(props.status))
</script>

<template>
  <span
    class="status-pill"
    :data-tone="tone"
  >
    {{ label }}
  </span>
</template>

<style scoped>
.status-pill {
  padding: 6px 10px;
  border-radius: var(--rr-radius-pill);
  font-size: 0.85rem;
  font-weight: 700;
  white-space: nowrap;
  background: rgb(148 163 184 / 0.12);
  color: var(--rr-color-text-secondary);
}

.status-pill[data-tone='positive'] {
  background: var(--rr-color-success-50);
  color: var(--rr-color-success-600);
}

.status-pill[data-tone='warning'] {
  background: var(--rr-color-warning-50);
  color: var(--rr-color-warning-600);
}

.status-pill[data-tone='negative'] {
  background: var(--rr-color-danger-50);
  color: var(--rr-color-danger-600);
}
</style>
