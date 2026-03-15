<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import type { RetrievalRunDetail } from 'src/boot/api'

import DebugPayload from './DebugPayload.vue'
import ReferenceList from './ReferenceList.vue'
import StatusPill from './StatusPill.vue'
import TokenListSection from './TokenListSection.vue'

const props = defineProps<{
  detail: RetrievalRunDetail
}>()

const { t } = useI18n()
</script>

<template>
  <article class="rr-panel diagnostics-panel">
    <div class="panel-header">
      <div>
        <p class="rr-kicker">{{ t('flow.search.diagnostics.kicker') }}</p>
        <h3>{{ t('flow.search.diagnostics.title') }}</h3>
        <p class="panel-subtitle">{{ t('flow.search.diagnostics.run', { id: props.detail.id }) }}</p>
      </div>
      <StatusPill :status="props.detail.answer_status" />
    </div>

    <div class="summary-grid">
      <div class="summary-item">
        <span class="summary-item__label">{{ t('flow.search.diagnostics.summary.topK') }}</span>
        <strong>{{ props.detail.top_k }}</strong>
      </div>
      <div class="summary-item">
        <span class="summary-item__label">{{ t('flow.search.diagnostics.summary.matches') }}</span>
        <strong>{{ props.detail.matched_chunk_ids.length }}</strong>
      </div>
      <div class="summary-item">
        <span class="summary-item__label">{{ t('flow.search.diagnostics.summary.references') }}</span>
        <strong>{{ props.detail.references.length }}</strong>
      </div>
    </div>

    <p
      v-if="props.detail.warning"
      class="rr-banner"
      data-tone="warning"
    >
      {{ t('flow.search.diagnostics.warning') }}: {{ props.detail.warning }}
    </p>

    <details class="trace-details">
      <summary>{{ t('flow.search.diagnostics.action') }}</summary>

      <div class="trace-details__body">
        <div class="section-block">
          <h4>{{ t('flow.search.diagnostics.question') }}</h4>
          <p class="answer-copy">{{ props.detail.query_text }}</p>
        </div>

        <div class="diagnostics-columns">
          <TokenListSection
            :title="t('flow.search.diagnostics.matchesTitle')"
            :empty-message="t('flow.search.diagnostics.matchesEmpty')"
            :items="props.detail.matched_chunk_ids"
          />
          <ReferenceList
            :title="t('flow.search.diagnostics.referencesTitle')"
            :description="t('flow.search.diagnostics.referencesDescription')"
            :empty-message="t('flow.search.diagnostics.referencesEmpty')"
            :references="props.detail.references"
          />
        </div>

        <DebugPayload
          :debug-json="props.detail.debug_json ?? {}"
          :summary-label="t('flow.search.diagnostics.debug')"
          :empty-message="t('flow.search.diagnostics.debugEmpty')"
        />
      </div>
    </details>
  </article>
</template>

<style scoped>
.diagnostics-panel {
  display: grid;
  gap: var(--rr-space-5);
}

.panel-header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.panel-header h3 {
  margin: 4px 0 0;
}

.panel-subtitle,
.summary-item__label {
  color: var(--rr-color-text-secondary);
}

.summary-grid,
.diagnostics-columns {
  display: grid;
  gap: var(--rr-space-3);
}

.summary-grid {
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
}

.diagnostics-columns {
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
}

.summary-item,
.section-block {
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-sm);
  background: rgb(255 255 255 / 0.64);
}

.summary-item {
  display: grid;
  gap: 4px;
}

.trace-details {
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.5);
}

.trace-details summary {
  cursor: pointer;
  font-weight: 700;
}

.trace-details__body {
  display: grid;
  gap: var(--rr-space-4);
  margin-top: var(--rr-space-4);
}

.answer-copy {
  margin: 0;
  white-space: pre-wrap;
  line-height: 1.5;
}

@media (width <= 700px) {
  .panel-header {
    flex-direction: column;
  }
}
</style>
