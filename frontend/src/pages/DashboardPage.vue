<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import { useFlowStore } from 'src/stores/flow'

interface WorkspaceItem {
  id: string
  slug: string
  name: string
}

interface ProjectItem {
  id: string
  slug: string
  name: string
}

const flowStore = useFlowStore()
const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

const hasWorkspace = computed(() => workspaces.value.length > 0)
const hasProject = computed(() => projects.value.length > 0)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === flowStore.workspaceId) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === flowStore.projectId) ?? null,
)

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  if (!flowStore.workspaceId && workspaces.value.length > 0) {
    flowStore.selectWorkspace(workspaces.value[0]?.id ?? '')
  }
  if (flowStore.workspaceId) {
    projects.value = await fetchProjects(flowStore.workspaceId)
    if (!flowStore.projectId && projects.value.length > 0) {
      flowStore.selectProject(projects.value[0]?.id ?? '')
    }
  }
})
</script>

<template>
  <section class="overview-page">
    <header class="hero-card">
      <p class="eyebrow">RustRAG minimal flow</p>
      <h2>Get from zero to a grounded answer</h2>
      <p>
        This UI is now focused on one path: create a workspace, create a project, ingest text,
        then ask questions against that project.
      </p>
    </header>

    <div class="overview-grid">
      <article class="step-card">
        <span class="step-card__index">1</span>
        <h3>Setup workspace and project</h3>
        <p>Create the basic containers RustRAG needs before ingestion and querying.</p>
        <p class="step-card__state">
          {{ hasWorkspace ? 'Workspace ready' : 'No workspace yet' }} ·
          {{ hasProject ? 'Project ready' : 'No project yet' }}
        </p>
        <RouterLink to="/setup">Open setup</RouterLink>
      </article>

      <article class="step-card">
        <span class="step-card__index">2</span>
        <h3>Ingest content</h3>
        <p>Paste text into the selected project and turn it into indexed chunks.</p>
        <p class="step-card__state">
          Current project:
          <strong>{{ selectedProject?.name ?? 'not selected' }}</strong>
        </p>
        <RouterLink to="/ingest">Open ingest</RouterLink>
      </article>

      <article class="step-card">
        <span class="step-card__index">3</span>
        <h3>Ask a question</h3>
        <p>Run a grounded query and inspect the answer with evidence references.</p>
        <p class="step-card__state">
          Workspace:
          <strong>{{ selectedWorkspace?.name ?? 'not selected' }}</strong>
        </p>
        <RouterLink to="/ask">Open ask</RouterLink>
      </article>
    </div>
  </section>
</template>

<style scoped>
.overview-page {
  display: grid;
  gap: 20px;
}

.hero-card,
.step-card {
  padding: 20px;
  border: 1px solid #d7dee7;
  border-radius: 16px;
  background: #f8fbff;
}

.eyebrow {
  margin: 0 0 8px;
  font-size: 0.8rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: #496280;
}

.hero-card h2,
.step-card h3 {
  margin-top: 0;
}

.overview-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 16px;
}

.step-card {
  display: grid;
  gap: 10px;
}

.step-card__index {
  display: inline-flex;
  width: 28px;
  height: 28px;
  align-items: center;
  justify-content: center;
  border-radius: 999px;
  background: #dbeafe;
  color: #1d4ed8;
  font-weight: 700;
}

.step-card__state {
  color: #526173;
}

.step-card a {
  width: fit-content;
  padding: 10px 14px;
  border-radius: 999px;
  background: #215dff;
  color: #fff;
  text-decoration: none;
  font-weight: 600;
}

@media (width <= 1100px) {
  .overview-grid {
    grid-template-columns: 1fr;
  }
}
</style>
