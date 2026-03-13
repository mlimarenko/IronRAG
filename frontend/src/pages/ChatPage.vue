<script setup lang="ts">
import { ref } from 'vue'

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

async function submitQuery() {
  loading.value = true
  errorMessage.value = null

  try {
    const response = await runQuery({
      project_id: projectId.value,
      query_text: queryText.value,
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
  <section>
    <h2>Chat / Query</h2>
    <p>LightRAG-inspired query experience with retrieved chunks, graph entities, and citations.</p>

    <div class="query-form">
      <input v-model="projectId" type="text" placeholder="Project ID" />
      <textarea v-model="queryText" rows="4" placeholder="Ask a grounded question" />
      <button type="button" :disabled="loading || !projectId || !queryText" @click="submitQuery">
        {{ loading ? 'Running…' : 'Run query' }}
      </button>
    </div>

    <p v-if="errorMessage">{{ errorMessage }}</p>

    <article v-if="result" class="result-panel">
      <h3>Answer</h3>
      <p>{{ result.answer }}</p>
      <p>Status: {{ result.answer_status }}</p>
      <p v-if="result.warning">Warning: {{ result.warning }}</p>
      <p>Weak grounding: {{ result.weak_grounding ? 'yes' : 'no' }}</p>

      <h4>References</h4>
      <ul>
        <li v-for="reference in result.references" :key="reference">
          {{ reference }}
        </li>
      </ul>
    </article>

    <article v-if="detail" class="result-panel">
      <h3>Retrieval diagnostics</h3>
      <p>Matched chunks: {{ detail.matched_chunk_ids.length }}</p>
      <p>Top K: {{ detail.top_k }}</p>
      <p>Answer status: {{ detail.answer_status }}</p>
      <p v-if="detail.warning">Diagnostic warning: {{ detail.warning }}</p>
    </article>
  </section>
</template>

<style scoped>
.query-form,
.result-panel {
  margin-top: 16px;
  padding: 16px;
  border: 1px solid #d7dee7;
  border-radius: 12px;
  background: #f8fbff;
}

.query-form {
  display: grid;
  gap: 12px;
}
</style>
