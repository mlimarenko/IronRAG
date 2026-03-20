<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type {
  DocumentGraphHealthSummary,
  DocumentsWorkspaceDiagnosticChip,
} from 'src/models/ui/documents'

defineProps<{
  chips: DocumentsWorkspaceDiagnosticChip[]
  graphHealth: DocumentGraphHealthSummary | null
  graphStatusLabel: string | null
  graphStatusMessage: string | null
}>()

const i18n = useI18n()

function chipHelp(kind: string): string | null {
  const key = `documents.workspace.chipHelp.${kind}`
  return i18n.te(key) ? i18n.t(key) : null
}
</script>

<template>
  <section
    v-if="chips.length || graphStatusMessage"
    class="rr-page-card rr-documents-diagnostics-strip"
  >
    <div
      v-if="chips.length"
      class="rr-documents-diagnostics-strip__chips"
    >
      <article
        v-for="chip in chips"
        :key="`${chip.kind}:${chip.label}`"
        class="rr-documents-diagnostics-strip__chip"
        :title="[`${chip.label}: ${chip.value}`, chipHelp(chip.kind)].filter(Boolean).join(' · ')"
        tabindex="0"
      >
        <span>{{ chip.label }}</span>
        <strong>{{ chip.value }}</strong>
      </article>
    </div>

    <div
      v-if="graphStatusMessage"
      class="rr-documents-diagnostics-strip__aside"
      :class="{
        'is-degraded':
          graphHealth?.projectionHealth === 'degraded' ||
          graphHealth?.projectionHealth === 'retrying_contention',
        'is-failed': graphHealth?.projectionHealth === 'failed',
      }"
      :title="graphStatusMessage"
    >
      <strong>{{ graphStatusLabel }}</strong>
      <span>{{ graphStatusMessage }}</span>
    </div>
  </section>
</template>
