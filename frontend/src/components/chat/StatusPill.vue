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
  border-radius: 999px;
  font-size: 0.85rem;
  font-weight: 700;
  white-space: nowrap;
  background: #e8edf3;
  color: #324253;
}

.status-pill[data-tone='positive'] {
  background: #dff7e6;
  color: #166534;
}

.status-pill[data-tone='warning'] {
  background: #fff4d8;
  color: #9a6700;
}

.status-pill[data-tone='negative'] {
  background: #fde2e2;
  color: #b42318;
}
</style>
