<script setup lang="ts">
import { onMounted, ref } from 'vue'

import {
  api,
  fetchIngestionJobDetail,
  retryIngestionJob,
  type IngestionJobDetail,
} from 'src/boot/api'

const jobs = ref<{ id: string; project_id: string; status: string; stage: string }[]>([])
const selectedJob = ref<IngestionJobDetail | null>(null)
const errorMessage = ref<string | null>(null)

async function loadJobDetail(id: string) {
  selectedJob.value = await fetchIngestionJobDetail(id)
}

async function handleRetry(id: string) {
  selectedJob.value = await retryIngestionJob(id)
}

onMounted(async () => {
  try {
    const { data } =
      await api.get<{ id: string; project_id: string; status: string; stage: string }[]>(
        '/v1/ingestion-jobs',
      )
    jobs.value = data

    if (data.length > 0) {
      await loadJobDetail(data[0].id)
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown ingestion error'
  }
})
</script>

<template>
  <section>
    <h2>Ingestion</h2>
    <p>Register sources, launch jobs, and inspect indexing state.</p>

    <p v-if="errorMessage">{{ errorMessage }}</p>
    <div
      v-else
      class="ingestion-grid"
    >
      <article class="panel">
        <h3>Jobs</h3>
        <ul>
          <li
            v-for="job in jobs"
            :key="job.id"
          >
            <button
              type="button"
              @click="loadJobDetail(job.id)"
            >
              {{ job.project_id }} — {{ job.status }} / {{ job.stage }}
            </button>
          </li>
        </ul>
      </article>

      <article class="panel">
        <h3>Selected job</h3>
        <p v-if="!selectedJob">No job selected.</p>
        <template v-else>
          <p>Status: {{ selectedJob.status }}</p>
          <p>Stage: {{ selectedJob.stage }}</p>
          <p>Lifecycle: {{ selectedJob.lifecycle }}</p>
          <p v-if="selectedJob.error_message">Error: {{ selectedJob.error_message }}</p>
          <button
            v-if="selectedJob.retryable"
            type="button"
            @click="handleRetry(selectedJob.id)"
          >
            Retry job
          </button>
        </template>
      </article>
    </div>
  </section>
</template>

<style scoped>
.ingestion-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
  margin-top: 16px;
}

.panel {
  padding: 16px;
  border: 1px solid #d7dee7;
  border-radius: 12px;
  background: #f8fbff;
}
</style>
