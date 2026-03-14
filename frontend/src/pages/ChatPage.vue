<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

import {
  fetchRetrievalRunDetail,
  runQuery,
  type QueryResponseSurface,
  type RetrievalRunDetail,
} from 'src/boot/api'
import RetrievalDiagnosticsPanel from 'src/components/chat/RetrievalDiagnosticsPanel.vue'
import StatusPill from 'src/components/chat/StatusPill.vue'
import TokenListSection from 'src/components/chat/TokenListSection.vue'
import { useFlowStore } from 'src/stores/flow'

const flowStore = useFlowStore()
const queryText = ref('')
const result = ref<QueryResponseSurface | null>(null)
const detail = ref<RetrievalRunDetail | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)

const selectedProject = flowStore.selectedProject
const selectedProjectId = flowStore.projectId

onMounted(async () => {
  await flowStore.bootstrap()
})

async function submitQuery() {
  loading.value = true
  errorMessage.value = null
  result.value = null
  detail.value = null
  try {
    if (!selectedProjectId) {
      throw new Error('Create and select a project in Setup before asking questions.')
    }

    const response = await runQuery({
      project_id: selectedProjectId,
      query_text: queryText.value.trim(),
      top_k: 8,
    })

    result.value = response
    try {
      detail.value = await fetchRetrievalRunDetail(response.retrieval_run_id)
    } catch {
      detail.value = null
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown query error'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="chat-page">
    <header>
      <h2>Ask</h2>
      <p>Run a grounded query against the currently selected project.</p>
    </header>

    <div class="context-card">
      <p><strong>Workspace:</strong> {{ flowStore.selectedWorkspace?.name ?? 'not selected' }}</p>
      <p><strong>Project:</strong> {{ selectedProject?.name ?? 'not selected' }}</p>
      <p v-if="!selectedProject">Go to Setup first, then ingest some text before querying.</p>
    </div>

    <div class="query-form card">
      <label class="field">
        <span class="field__label">Question</span>
        <textarea
          v-model="queryText"
          rows="4"
          placeholder="Ask a grounded question about the indexed content"
        />
      </label>

      <div class="query-form__actions">
        <button
          type="button"
          :disabled="loading || !selectedProjectId || !queryText.trim()"
          @click="submitQuery"
        >
          {{ loading ? 'Running…' : 'Run query' }}
        </button>
      </div>
    </div>

    <p v-if="errorMessage" class="error-banner">
      {{ errorMessage }}
    </p>

    <article v-if="result" class="card result-panel">
      <div class="panel-header">
        <div>
          <h3>Answer</h3>
          <p class="panel-subtitle">Final response from the query endpoint.</p>
        </div>
        <StatusPill :status="result.answer_status" />
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
          <strong>{{ result.references.length }}</strong>
        </div>
      </div>

      <p class="answer-copy">{{ result.answer }}</p>

      <p v-if="result.warning" class="warning-banner">
        Warning: {{ result.warning }}
      </p>

      <TokenListSection
        title="Evidence references"
        empty-message="No references were returned for this answer."
        :items="result.references"
      />
    </article>

    <RetrievalDiagnosticsPanel v-if="detail" :detail="detail" />
  </section>
</template>

<style scoped>
.chat-page {
  display: grid;
  gap: 16px;
}

.context-card,
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
.panel-subtitle {
  color: #526173;
}

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

.summary-grid {
  display: grid;
  gap: 12px;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
}

.summary-item {
  display: grid;
  gap: 4px;
  padding: 12px;
  border-radius: 10px;
  background: rgb(255 255 255 / 65%);
}

.answer-copy {
  margin: 0;
  white-space: pre-wrap;
  line-height: 1.5;
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
</style>
