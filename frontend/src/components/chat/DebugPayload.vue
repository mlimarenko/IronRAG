<script setup lang="ts">
import { computed } from 'vue'

import { formatDebugEntries } from './statusFormatting'

const props = defineProps<{
  debugJson: Record<string, unknown>
  summaryLabel: string
  emptyMessage: string
}>()

const debugEntries = computed(() => formatDebugEntries(props.debugJson))
</script>

<template>
  <details class="debug-block">
    <summary>{{ summaryLabel }} ({{ debugEntries.length }})</summary>
    <p
      v-if="!debugEntries.length"
      class="muted"
    >
      {{ emptyMessage }}
    </p>
    <dl
      v-else
      class="debug-list"
    >
      <div
        v-for="entry in debugEntries"
        :key="entry.key"
        class="debug-list__row"
      >
        <dt>{{ entry.key }}</dt>
        <dd>
          <pre>{{ entry.preview }}</pre>
        </dd>
      </div>
    </dl>
  </details>
</template>

<style scoped>
.debug-block {
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(16 24 40 / 0.03);
}

.debug-block summary {
  cursor: pointer;
  font-weight: 600;
}

.muted {
  color: var(--rr-color-text-secondary);
}

.debug-list {
  margin: var(--rr-space-3) 0 0;
}

.debug-list__row {
  display: grid;
  gap: var(--rr-space-2);
  padding-top: var(--rr-space-3);
  border-top: 1px solid var(--rr-color-border-subtle);
}

.debug-list__row:first-child {
  border-top: 0;
  padding-top: 0;
}

.debug-list__row dt {
  font-weight: 700;
}

.debug-list__row dd {
  margin: 0;
}

.debug-list__row pre {
  margin: 0;
  padding: var(--rr-space-3);
  border-radius: var(--rr-radius-sm);
  overflow-x: auto;
  background: var(--rr-color-bg-contrast);
  color: var(--rr-color-text-inverse);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
