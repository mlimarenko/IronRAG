<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  setSelectedWorkspaceId,
} from 'src/stores/flow'

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

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

const hasWorkspace = computed(() => workspaces.value.length > 0)
const hasProject = computed(() => projects.value.length > 0)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const nextAction = computed(() => {
  if (!hasWorkspace.value) {
    return 'Create the first workspace in Setup.'
  }

  if (!hasProject.value) {
    return 'Create or select a project in Setup.'
  }

  return 'Open Ingest and index the first text sample.'
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = getSelectedWorkspaceId()
  if (!workspaceId && workspaces.value.length > 0) {
    setSelectedWorkspaceId(workspaces.value[0]?.id ?? '')
  }
  const activeWorkspaceId = getSelectedWorkspaceId()
  if (activeWorkspaceId) {
    projects.value = await fetchProjects(activeWorkspaceId)
    if (!getSelectedProjectId() && projects.value.length > 0) {
      setSelectedProjectId(projects.value[0]?.id ?? '')
    }
  }
})
</script>

<template>
  <section class="rr-page-grid overview-page">
    <PageSection
      eyebrow="Minimal flow"
      title="Get from zero to a grounded answer"
      description="The shell is now focused on one practical path: establish workspace context, ingest content into a project, then query it with evidence-aware answers."
      status="focused"
      status-label="Four-step operator flow"
    >
      <template #actions>
        <RouterLink class="rr-button" to="/setup">
          Start with setup
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">Active workspace</p>
          <strong>{{ selectedWorkspace?.name ?? 'Not selected yet' }}</strong>
          <p>{{ hasWorkspace ? 'Selection is persisted across the flow.' : 'Create one to unlock projects.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Active project</p>
          <strong>{{ selectedProject?.name ?? 'Not selected yet' }}</strong>
          <p>{{ hasProject ? 'Ingest and Ask use the same project context.' : 'Projects appear after workspace selection.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Next action</p>
          <strong>{{ nextAction }}</strong>
          <p>The Overview page stays descriptive and keeps the practical path explicit.</p>
        </article>
      </div>

      <div class="rr-grid rr-grid--cards">
        <article class="flow-card rr-panel" data-state="setup">
          <div class="flow-card__header">
            <span class="flow-card__step">1</span>
            <StatusBadge
              :status="hasProject ? 'ready' : hasWorkspace ? 'partial' : 'draft'"
              :label="hasProject ? 'Project selected' : hasWorkspace ? 'Workspace selected' : 'Needs setup'"
            />
          </div>
          <div class="flow-card__body">
            <h3>Setup workspace and project</h3>
            <p>Create the containers RustRAG needs and keep one project selected across the flow.</p>
          </div>
          <div class="flow-card__footer">
            <p>Workspace: <strong>{{ selectedWorkspace?.name ?? 'none' }}</strong></p>
            <RouterLink class="rr-button rr-button--secondary" to="/setup">
              Open setup
            </RouterLink>
          </div>
        </article>

        <article class="flow-card rr-panel" data-state="ingest">
          <div class="flow-card__header">
            <span class="flow-card__step">2</span>
            <StatusBadge
              :status="selectedProject ? 'ready' : 'blocked'"
              :label="selectedProject ? 'Project ready' : 'Select a project first'"
            />
          </div>
          <div class="flow-card__body">
            <h3>Ingest content</h3>
            <p>Paste text into the selected project and turn it into indexed chunks for retrieval.</p>
          </div>
          <div class="flow-card__footer">
            <p>Project: <strong>{{ selectedProject?.name ?? 'none' }}</strong></p>
            <RouterLink class="rr-button rr-button--secondary" to="/ingest">
              Open ingest
            </RouterLink>
          </div>
        </article>

        <article class="flow-card rr-panel" data-state="ask">
          <div class="flow-card__header">
            <span class="flow-card__step">3</span>
            <StatusBadge
              :status="selectedProject ? 'ready' : 'blocked'"
              :label="selectedProject ? 'Ready to query' : 'Needs project context'"
            />
          </div>
          <div class="flow-card__body">
            <h3>Ask a grounded question</h3>
            <p>Run a query against the indexed content and inspect answer quality with supporting references.</p>
          </div>
          <div class="flow-card__footer">
            <p>Workspace: <strong>{{ selectedWorkspace?.name ?? 'none' }}</strong></p>
            <RouterLink class="rr-button rr-button--secondary" to="/ask">
              Open ask
            </RouterLink>
          </div>
        </article>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.flow-card {
  gap: var(--rr-space-5);
}

.flow-card__header,
.flow-card__footer {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
}

.flow-card__body {
  display: grid;
  gap: var(--rr-space-3);
}

.flow-card__body h3,
.flow-card__body p,
.flow-card__footer p {
  margin: 0;
}

.flow-card__footer {
  flex-wrap: wrap;
  align-items: flex-end;
}

.flow-card__step {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 2.1rem;
  height: 2.1rem;
  border-radius: var(--rr-radius-pill);
  background: var(--rr-color-accent-50);
  color: var(--rr-color-accent-700);
  font-weight: 700;
}

.flow-card[data-state='ask'] .flow-card__step {
  background: rgb(14 165 233 / 0.12);
  color: #0369a1;
}

@media (width <= 900px) {
  .flow-card__header,
  .flow-card__footer {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
