<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { fetchUsageSummary, type UsageSummary } from 'src/boot/api'

const summary = ref<UsageSummary | null>(null)
const errorMessage = ref<string | null>(null)
const infoMessage = ref<string | null>(null)

function extractErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'Unknown dashboard error'
}

function isUnauthorizedMessage(message: string): boolean {
  const normalized = message.toLowerCase()
  return normalized.includes('401') || normalized.includes('unauthorized') || normalized.includes('authorization')
}

onMounted(async () => {
  try {
    summary.value = await fetchUsageSummary()
  } catch (error) {
    const message = extractErrorMessage(error)
    if (isUnauthorizedMessage(message)) {
      infoMessage.value = 'Usage summary requires an authorized API token. Public shell is up; protected metrics are hidden.'
    } else {
      errorMessage.value = message
    }
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
    <div
      v-else-if="summary"
      class="summary-grid"
    >
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
    <div
      v-else
      class="summary-card"
    >
      <h3>Public shell is online</h3>
      <p>{{ infoMessage ?? 'Protected metrics are unavailable until an API token is configured in the UI flow.' }}</p>
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
