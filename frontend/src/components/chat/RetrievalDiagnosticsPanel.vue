<script setup lang="ts">
import type { RetrievalRunDetail } from 'src/boot/api'

import DebugPayload from './DebugPayload.vue'
import StatusPill from './StatusPill.vue'
import TokenListSection from './TokenListSection.vue'

defineProps<{
  detail: RetrievalRunDetail
}>()
</script>

<template>
  <article class="rr-panel diagnostics-panel">
    <div class="panel-header">
      <div>
        <p class="rr-kicker">Retrieval trace</p>
        <h3>Retrieval diagnostics</h3>
        <p class="panel-subtitle">Detail from retrieval run {{ detail.id }}.</p>
      </div>
      <StatusPill :status="detail.answer_status" />
    </div>

    <div class="summary-grid">
      <div class="summary-item">
        <span class="summary-item__label">Top K</span>
        <strong>{{ detail.top_k }}</strong>
      </div>
      <div class="summary-item">
        <span class="summary-item__label">Matched chunks</span>
        <strong>{{ detail.matched_chunk_ids.length }}</strong>
      </div>
      <div class="summary-item">
        <span class="summary-item__label">Grounding</span>
        <strong>{{ detail.weak_grounding ? 'Weak' : 'OK' }}</strong>
      </div>
    </div>

    <div class="section-block">
      <h4>Query text</h4>
      <p class="answer-copy">{{ detail.query_text }}</p>
    </div>

    <p
      v-if="detail.warning"
      class="warning-banner"
    >
      Diagnostic warning: {{ detail.warning }}
    </p>

    <div class="diagnostics-columns">
      <TokenListSection
        title="Matched chunk IDs"
        empty-message="No chunk matches were recorded."
        :items="detail.matched_chunk_ids"
      />
      <TokenListSection
        title="Recorded references"
        empty-message="No references were stored on the retrieval run."
        :items="detail.references"
      />
    </div>

    <DebugPayload :debug-json="detail.debug_json ?? {}" />
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
