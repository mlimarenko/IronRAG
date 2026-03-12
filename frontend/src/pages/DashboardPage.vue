<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { fetchUsageSummary, type UsageSummary } from 'src/boot/api'

const summary = ref<UsageSummary | null>(null)
const errorMessage = ref<string | null>(null)

onMounted(async () => {
  try {
    summary.value = await fetchUsageSummary()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown dashboard error'
  }
})
</script>

<template>
  <section>
    <h2>Dashboard</h2>
    <p>
      RustRAG instance overview: workspaces, projects, ingestion activity, and recent query runs.
    </p>

    <p v-if="errorMessage">{{ errorMessage }}</p>
    <div v-else-if="summary" class="summary-grid">
      <article class="summary-card">
        <h3>Usage events</h3>
        <p>{{ summary.usage_events }}</p>
      </article>
      <article class="summary-card">
        <h3>Total tokens</h3>
        <p>{{ summary.total_tokens }}</p>
      </article>
      <article class="summary-card">
        <h3>Estimated cost</h3>
        <p>{{ summary.estimated_cost }}</p>
      </article>
    </div>
  </section>
</template>

<style scoped>
.summary-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 16px;
  margin-top: 16px;
}

.summary-card {
  padding: 16px;
  border: 1px solid #d7dee7;
  border-radius: 12px;
  background: #f8fbff;
}
</style>
