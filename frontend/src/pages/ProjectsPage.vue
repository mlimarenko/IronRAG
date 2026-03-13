<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { api, fetchProjectReadiness, type ProjectReadinessSummary } from 'src/boot/api'

const projects = ref<{ id: string; name: string; slug: string }[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const errorMessage = ref<string | null>(null)

async function loadReadiness(id: string) {
  readiness.value = await fetchProjectReadiness(id)
}

onMounted(async () => {
  try {
    const { data } = await api.get<{ id: string; name: string; slug: string }[]>('/v1/projects')
    projects.value = data

    if (data.length > 0) {
      await loadReadiness(data[0].id)
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown project error'
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
        <ul>
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
        <p v-if="!readiness">No readiness data loaded.</p>
        <template v-else>
          <p>Indexing state: {{ readiness.indexing_state }}</p>
          <p>Ready for query: {{ readiness.ready_for_query ? 'yes' : 'no' }}</p>
          <p>Sources: {{ readiness.sources }}</p>
          <p>Documents: {{ readiness.documents }}</p>
          <p>Ingestion jobs: {{ readiness.ingestion_jobs }}</p>
        </template>
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
