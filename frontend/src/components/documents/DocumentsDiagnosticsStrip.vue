<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  DocumentGraphHealthSummary,
  DocumentsWorkspaceDiagnosticChip,
} from 'src/models/ui/documents'

defineProps<{
  chips: DocumentsWorkspaceDiagnosticChip[]
  graphBackend: string | null
  graphHealth: DocumentGraphHealthSummary | null
  graphStatus: string | null
  graphStatusLabel: string | null
  graphStatusMessage: string | null
}>()

const i18n = useI18n()
const { humanizeToken } = useDisplayFormatters()

function chipHelp(kind: string): string | null {
  const key = `documents.workspace.chipHelp.${kind}`
  return i18n.te(key) ? i18n.t(key) : null
}

function chipLabel(kind: string, fallback: string): string {
  const key = `documents.workspace.chipLabels.${kind}`
  if (i18n.te(key)) {
    return i18n.t(key)
  }
  if (fallback.trim()) {
    return humanizeToken(fallback)
  }
  return humanizeToken(kind)
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
        :title="[`${chipLabel(chip.kind, chip.label)}: ${chip.value}`, chipHelp(chip.kind)].filter(Boolean).join(' · ')"
        tabindex="0"
      >
        <span>{{ chipLabel(chip.kind, chip.label) }}</span>
        <strong>{{ chip.value }}</strong>
      </article>
    </div>

      <div
        v-if="graphStatusMessage"
        class="rr-documents-diagnostics-strip__aside"
        :class="{
          'is-degraded':
            graphStatus === 'building' ||
            graphStatus === 'partial' ||
            graphStatus === 'rebuilding' ||
            graphStatus === 'stale',
          'is-failed': graphStatus === 'failed',
        }"
        :title="graphStatusMessage"
      >
        <strong>{{ graphStatusLabel }}</strong>
        <span
          v-if="graphBackend"
          class="rr-documents-diagnostics-strip__backend"
        >
          {{ $t('documents.workspace.knowledgePlane', { backend: graphBackend }) }}
        </span>
        <span>{{ graphStatusMessage }}</span>
      </div>
    </section>
</template>
