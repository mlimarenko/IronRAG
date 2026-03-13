<script setup lang="ts">
import { computed, ref } from 'vue'

import {
  fetchRetrievalRunDetail,
  runQuery,
  type QueryResponseSurface,
  type RetrievalRunDetail,
} from 'src/boot/api'

const projectId = ref('')
const queryText = ref('')
const result = ref<QueryResponseSurface | null>(null)
const detail = ref<RetrievalRunDetail | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)

const answerStatusTone = computed(() => getStatusTone(result.value?.answer_status))
const detailStatusTone = computed(() => getStatusTone(detail.value?.answer_status))
const referenceCount = computed(() => result.value?.references.length ?? 0)
const matchedChunkCount = computed(() => detail.value?.matched_chunk_ids.length ?? 0)
const debugEntries = computed(() => formatDebugEntries(detail.value?.debug_json ?? {}))

function getStatusTone(status?: string): 'positive' | 'warning' | 'negative' | 'neutral' {
  const normalized = status?.toLowerCase() ?? ''

  if (['grounded', 'complete', 'ok', 'success'].includes(normalized)) {
    return 'positive'
  }

  if (
    ['partial', 'weakly_grounded', 'weak', 'degraded', 'warning', 'fallback'].includes(normalized)
  ) {
    return 'warning'
  }

  if (['failed', 'error', 'ungrounded', 'empty', 'blocked'].includes(normalized)) {
    return 'negative'
  }

  return 'neutral'
}

function formatStatusLabel(status?: string): string {
  if (!status) {
    return 'Unknown'
  }

  return status
    .split(/[_-]/g)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

function formatDebugEntries(debugJson: Record<string, unknown>) {
  return Object.entries(debugJson).map(([key, value]) => ({
    key,
    preview: formatDebugValue(value),
  }))
}

function formatDebugValue(value: unknown): string {
  if (value == null) {
    return 'null'
  }

  if (typeof value === 'string') {
    return value
  }

  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value)
  }

  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return '[unserializable value]'
  }
}

async function submitQuery() {
  loading.value = true
  errorMessage.value = null
  result.value = null
  detail.value = null
  try {
    const response = await runQuery({
      project_id: projectId.value.trim(),
      query_text: queryText.value.trim(),
      top_k: 8,
    })

    result.value = response
    detail.value = await fetchRetrievalRunDetail(response.retrieval_run_id)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown query error'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="chat-page">
    <h2>Chat / Query</h2>
    <p>LightRAG-inspired query experience with retrieved chunks, graph entities, and citations.</p>

    <div class="query-form card">
      <label class="field">
        <span class="field__label">Project ID</span>
        <input
          v-model="projectId"
          type="text"
          placeholder="Project UUID"
        >
      </label>

      <label class="field">
        <span class="field__label">Question</span>
        <textarea
          v-model="queryText"
          rows="4"
          placeholder="Ask a grounded question"
        />
      </label>

      <div class="query-form__actions">
        <button
          type="button"
          :disabled="loading || !projectId || !queryText"
          @click="submitQuery"
        >
          {{ loading ? 'Running…' : 'Run query' }}
        </button>
      </div>
    </div>

    <p
      v-if="errorMessage"
      class="error-banner"
    >
      {{ errorMessage }}
    </p>

    <article
      v-if="result"
      class="card result-panel"
    >
      <div class="panel-header">
        <div>
          <h3>Answer</h3>
          <p class="panel-subtitle">Final response from the query endpoint.</p>
        </div>
        <span
          class="status-pill"
          :data-tone="answerStatusTone"
        >
          {{ formatStatusLabel(result.answer_status) }}
        </span>
      </div>

      <div class="summary-grid">
        <div class="summary-item">
          <span class="summary-item__label">Mode</span>
          <strong>{{ result.mode }}</strong>
        </div>
        <div class="summary-item">
          <span class="summary-item__label">Grounding</span>
          <strong>{{ result.weak_grounding ? 'Weak' : 'OK' }}</strong>
        </div>
        <div class="summary-item">
          <span class="summary-item__label">References</span>
          <strong>{{ referenceCount }}</strong>
        </div>
      </div>

      <p class="answer-copy">{{ result.answer }}</p>

      <p
        v-if="result.warning"
        class="warning-banner"
      >
        Warning: {{ result.warning }}
      </p>

      <div class="section-block">
        <h4>Evidence references</h4>
        <p
          v-if="!result.references.length"
          class="muted"
        >
          No references were returned for this answer.
        </p>
        <ul
          v-else
          class="token-list"
        >
          <li
            v-for="reference in result.references"
            :key="reference"
          >
            <code>{{ reference }}</code>
          </li>
        </ul>
      </div>
    </article>

    <article
      v-if="detail"
      class="card result-panel diagnostics-panel"
    >
      <div class="panel-header">
        <div>
          <h3>Retrieval diagnostics</h3>
          <p class="panel-subtitle">Detail from retrieval run {{ detail.id }}.</p>
        </div>
        <span
          class="status-pill"
          :data-tone="detailStatusTone"
        >
          {{ formatStatusLabel(detail.answer_status) }}
        </span>
      </div>

      <div class="summary-grid">
        <div class="summary-item">
          <span class="summary-item__label">Top K</span>
          <strong>{{ detail.top_k }}</strong>
        </div>
        <div class="summary-item">
          <span class="summary-item__label">Matched chunks</span>
          <strong>{{ matchedChunkCount }}</strong>
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
        <div class="section-block">
          <h4>Matched chunk IDs</h4>
          <p
            v-if="!detail.matched_chunk_ids.length"
            class="muted"
          >
            No chunk matches were recorded.
          </p>
          <ul
            v-else
            class="token-list"
          >
            <li
              v-for="chunkId in detail.matched_chunk_ids"
              :key="chunkId"
            >
              <code>{{ chunkId }}</code>
            </li>
          </ul>
        </div>

        <div class="section-block">
          <h4>Recorded references</h4>
          <p
            v-if="!detail.references.length"
            class="muted"
          >
            No references were stored on the retrieval run.
          </p>
          <ul
            v-else
            class="token-list"
          >
            <li
              v-for="reference in detail.references"
              :key="reference"
            >
              <code>{{ reference }}</code>
            </li>
          </ul>
        </div>
      </div>

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
    </article>
  </section>
</template>

<style scoped>
.chat-page {
  display: grid;
  gap: 16px;
}

.card {
  padding: 16px;
  border: 1px solid #d7dee7;
  border-radius: 12px;
  background: #f8fbff;
}

.query-form {
  display: grid;
  gap: 12px;
}

.field {
  display: grid;
  gap: 6px;
}

.field__label,
.summary-item__label,
.panel-subtitle,
.muted {
  color: #526173;
}

.query-form input,
.query-form textarea {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #c8d5e3;
  border-radius: 10px;
  font: inherit;
  background: #fff;
}

.query-form__actions {
  display: flex;
  justify-content: flex-start;
}

.query-form button {
  padding: 10px 16px;
  border: 0;
  border-radius: 999px;
  background: #215dff;
  color: #fff;
  font: inherit;
  font-weight: 600;
  cursor: pointer;
}

.query-form button:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}

.result-panel {
  display: grid;
  gap: 16px;
}

.panel-header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
}

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

.summary-grid,
.diagnostics-columns {
  display: grid;
  gap: 12px;
}

.summary-grid {
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
}

.diagnostics-columns {
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
}

.summary-item,
.section-block {
  padding: 12px;
  border-radius: 10px;
  background: rgb(255 255 255 / 65%);
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

.token-list {
  display: grid;
  gap: 8px;
  padding-left: 20px;
  margin: 0;
}

.token-list code {
  overflow-wrap: anywhere;
}

.warning-banner,
.error-banner {
  padding: 12px 14px;
  border-radius: 10px;
}

.warning-banner {
  background: #fff4d8;
  color: #7c5600;
}

.error-banner {
  background: #fde2e2;
  color: #b42318;
}

.debug-block {
  padding: 12px;
  border-radius: 10px;
  background: rgb(16 24 40 / 0.04);
}

.debug-block summary {
  cursor: pointer;
  font-weight: 600;
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
