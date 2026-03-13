<script setup lang="ts">
import { computed } from 'vue'

import { formatDebugEntries } from './statusFormatting'

const props = defineProps<{
  debugJson: Record<string, unknown>
}>()

const debugEntries = computed(() => formatDebugEntries(props.debugJson))
</script>

<template>
  <details class="debug-block">
    <summary>Raw debug payload ({{ debugEntries.length }} entries)</summary>
    <p
      v-if="!debugEntries.length"
      class="muted"
    >
      No debug payload was returned.
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
  padding: 12px;
  border-radius: 10px;
  background: rgb(16 24 40 / 0.04);
}

.debug-block summary {
  cursor: pointer;
  font-weight: 600;
}

.muted {
  color: #526173;
}

.debug-list {
  margin: 12px 0 0;
}

.debug-list__row {
  display: grid;
  gap: 8px;
  padding-top: 12px;
  border-top: 1px solid #d7dee7;
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
  padding: 10px;
  border-radius: 8px;
  overflow-x: auto;
  background: #111827;
  color: #f9fafb;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
