<script setup lang="ts">
import DocumentSummaryCard from './DocumentSummaryCard.vue'
import type {
  DocumentsWorkspacePrimarySummary,
  DocumentStatus,
} from 'src/models/ui/documents'

defineProps<{
  primarySummary: DocumentsWorkspacePrimarySummary | null
  summaryCards: { tone: DocumentStatus; value: number; label: string }[]
  terminalBanner: {
    tone: DocumentStatus
    title: string
    summary: string
    chips: string[]
  } | null
  supportingLines: string[]
}>()
</script>

<template>
  <section class="rr-page-card rr-documents-primary-summary">
    <div
      v-if="primarySummary"
      class="rr-documents-primary-summary__labels"
    >
      <article>
        <span>{{ $t('documents.workspace.primary.progress') }}</span>
        <strong>{{ primarySummary.progressLabel }}</strong>
      </article>
      <article>
        <span>{{ $t('documents.workspace.primary.spend') }}</span>
        <strong>{{ primarySummary.spendLabel }}</strong>
      </article>
      <article>
        <span>{{ $t('documents.workspace.primary.backlog') }}</span>
        <strong>{{ primarySummary.backlogLabel }}</strong>
      </article>
    </div>

    <div
      v-if="summaryCards.length"
      class="rr-documents__summary rr-documents__summary--compact"
    >
      <article
        v-for="card in summaryCards"
        :key="card.label"
      >
        <DocumentSummaryCard
          :tone="card.tone"
          :value="card.value"
          :label="card.label"
        />
      </article>
    </div>

    <section
      v-if="terminalBanner"
      class="rr-documents__terminal-banner"
      :class="`is-${terminalBanner.tone}`"
    >
      <div class="rr-documents__terminal-banner-head">
        <strong>{{ terminalBanner.title }}</strong>
      </div>
      <p>{{ terminalBanner.summary }}</p>
      <div
        v-if="terminalBanner.chips.length"
        class="rr-documents__terminal-banner-chips"
      >
        <span
          v-for="chip in terminalBanner.chips"
          :key="chip"
          class="rr-documents__terminal-banner-chip"
        >
          {{ chip }}
        </span>
      </div>
    </section>

    <div
      v-if="supportingLines.length"
      class="rr-documents-primary-summary__supporting"
    >
      <p
        v-for="line in supportingLines"
        :key="line"
      >
        {{ line }}
      </p>
    </div>
  </section>
</template>
