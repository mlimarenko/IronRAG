<script setup lang="ts">
import type { DocumentStatus } from 'src/models/ui/documents'

const props = defineProps<{
  tone: DocumentStatus
  value: string | number
  label: string
}>()
</script>

<template>
  <article
    class="rr-summary-card"
    :class="`rr-summary-card--${props.tone}`"
  >
    <div>
      <strong>{{ props.value }}</strong>
      <span>{{ props.label }}</span>
    </div>
  </article>
</template>
