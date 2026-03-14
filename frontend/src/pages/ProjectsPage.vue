<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { api, fetchProjectReadiness, type ProjectReadinessSummary } from 'src/boot/api'

const projects = ref<{ id: string; name: string; slug: string }[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const errorMessage = ref<string | null>(null)
const infoMessage = ref<string | null>(null)

function extractErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'Unknown project error'
}

function isUnauthorizedMessage(message: string): boolean {
  const normalized = message.toLowerCase()
  return normalized.includes('401') || normalized.includes('unauthorized') || normalized.includes('authorization')
}

async function loadReadiness(id: string) {
  errorMessage.value = null
  infoMessage.value = null

  try {
    readiness.value = await fetchProjectReadiness(id)
  } catch (error) {
    const message = extractErrorMessage(error)
    if (isUnauthorizedMessage(message)) {
      infoMessage.value = 'Project list is visible, but readiness details require an authorized API token.'
      readiness.value = null
    } else {
      errorMessage.value = message
    }
  }
}

onMounted(async () => {
  try {
    const { data } = await api.get<{ id: string; name: string; slug: string }[]>('/projects')
    projects.value = data

    if (data.length === 0) {
      infoMessage.value = 'No projects created yet. Create a workspace and project to start ingestion and retrieval.'
      return
    }

    await loadReadiness(data[0].id)
  } catch (error) {
    errorMessage.value = extractErrorMessage(error)
  }
})
</script>

<template>
  <section>
    <h2>Projects</h2>
    <p>Projects are the primary RAG work surface inside a workspace.</p>

    <p v-if="errorMessage">{{ errorMessage }}</p>
    <div
      v-else
      class="projects-grid"
    >
      <article class="panel">
        <h3>Projects</h3>
        <p v-if="projects.length === 0">{{ infoMessage ?? 'No projects found yet.' }}</p>
        <ul v-else>
          <li
            v-for="project in projects"
            :key="project.id"
          >
            <button
              type="button"
              @click="loadReadiness(project.id)"
            >
              {{ project.name }} ({{ project.slug }})
            </button>
          </li>
        </ul>
      </article>

      <article class="panel">
        <h3>Readiness</h3>
        <p v-if="readiness">
          Indexing state: {{ readiness.indexing_state }}
        </p>
        <template v-if="readiness">
          <p>Ready for query: {{ readiness.ready_for_query ? 'yes' : 'no' }}</p>
          <p>Sources: {{ readiness.sources }}</p>
          <p>Documents: {{ readiness.documents }}</p>
          <p>Ingestion jobs: {{ readiness.ingestion_jobs }}</p>
        </template>
        <p v-else>
          {{ infoMessage ?? 'No readiness data loaded.' }}
        </p>
      </article>
    </div>
  </section>
</template>

<style scoped>
.projects-grid {
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
