<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { createProject, createWorkspace, fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  resetSelectedProjectId,
  setSelectedProjectId,
  setSelectedWorkspaceId,
} from 'src/stores/flow'

interface WorkspaceItem {
  id: string
  slug: string
  name: string
  status?: string
}

interface ProjectItem {
  id: string
  slug: string
  name: string
  workspace_id: string
  description?: string | null
}

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const workspaceForm = ref({ slug: '', name: '' })
const projectForm = ref({ slug: '', name: '', description: '' })
const workspaceError = ref<string | null>(null)
const projectError = ref<string | null>(null)
const successMessage = ref<string | null>(null)

const selectedWorkspaceId = computed({
  get: () => getSelectedWorkspaceId(),
  set: (value: string) => {
    setSelectedWorkspaceId(value)
  },
})

const selectedProjectId = computed({
  get: () => getSelectedProjectId(),
  set: (value: string) => {
    setSelectedProjectId(value)
  },
})

const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === selectedWorkspaceId.value) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === selectedProjectId.value) ?? null,
)
const setupStatus = computed(() => {
  if (selectedProject.value) {
    return { status: 'ready', label: 'Project selected' }
  }

  if (selectedWorkspace.value) {
    return { status: 'partial', label: 'Workspace selected' }
  }

  return { status: 'draft', label: 'Create your first workspace' }
})

watch(selectedWorkspaceId, async (value) => {
  resetSelectedProjectId()
  projects.value = value ? await fetchProjects(value) : []
  if (!getSelectedProjectId() && projects.value.length > 0) {
    setSelectedProjectId(projects.value[0]?.id ?? '')
  }
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  if (!getSelectedWorkspaceId() && workspaces.value.length > 0) {
    setSelectedWorkspaceId(workspaces.value[0]?.id ?? '')
  }
  const workspaceId = getSelectedWorkspaceId()
  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    if (!getSelectedProjectId() && projects.value.length > 0) {
      setSelectedProjectId(projects.value[0]?.id ?? '')
    }
  }
})

async function createWorkspaceItem() {
  workspaceError.value = null
  successMessage.value = null
  try {
    const created = await createWorkspace({
      slug: workspaceForm.value.slug.trim(),
      name: workspaceForm.value.name.trim(),
    })
    workspaces.value = [created, ...workspaces.value.filter((item) => item.id !== created.id)]
    workspaceForm.value = { slug: '', name: '' }
    setSelectedWorkspaceId(created.id)
    successMessage.value = `Workspace ${created.name} created.`
  } catch (error) {
    workspaceError.value = error instanceof Error ? error.message : 'Failed to create workspace'
  }
}

async function createProjectItem() {
  projectError.value = null
  successMessage.value = null
  if (!getSelectedWorkspaceId()) {
    projectError.value = 'Select or create a workspace first.'
    return
  }

  try {
    const created = await createProject({
      workspace_id: getSelectedWorkspaceId(),
      slug: projectForm.value.slug.trim(),
      name: projectForm.value.name.trim(),
      description: projectForm.value.description.trim() || null,
    })
    projects.value = [created, ...projects.value.filter((item) => item.id !== created.id)]
    projectForm.value = { slug: '', name: '', description: '' }
    setSelectedProjectId(created.id)
    successMessage.value = `Project ${created.name} created.`
  } catch (error) {
    projectError.value = error instanceof Error ? error.message : 'Failed to create project'
  }
}
</script>

<template>
  <section class="rr-page-grid setup-page">
    <PageSection
      eyebrow="Step 1"
      title="Set up workspace and project context"
      description="Choose the active workspace first, then keep one project selected for ingestion and grounded querying."
      :status="setupStatus.status"
      :status-label="setupStatus.label"
    >
      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">Workspaces</p>
          <strong>{{ workspaces.length }}</strong>
          <p>{{ selectedWorkspace?.name ?? 'No active workspace selected yet.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Projects in scope</p>
          <strong>{{ projects.length }}</strong>
          <p>{{ selectedProject?.name ?? 'Pick or create one for the minimal flow.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Next step</p>
          <strong>{{ selectedProject ? 'Continue to ingest' : 'Finish project setup' }}</strong>
          <p>The selection made here becomes the default context for the next pages.</p>
        </article>
      </div>

      <p
        v-if="successMessage"
        class="rr-banner"
        data-tone="success"
      >
        {{ successMessage }}
      </p>

      <div class="setup-grid">
        <article class="rr-panel rr-panel--accent setup-panel">
          <div class="setup-panel__heading">
            <div>
              <p class="rr-kicker">Workspace</p>
              <h3>Create or select a workspace</h3>
            </div>
            <StatusBadge
              :status="selectedWorkspace ? 'ready' : 'draft'"
              :label="selectedWorkspace ? 'Selected' : 'Required'"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">Selected workspace</span>
              <select v-model="selectedWorkspaceId" class="rr-control">
                <option value="">Choose workspace</option>
                <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
                  {{ workspace.name }} ({{ workspace.slug }})
                </option>
              </select>
              <p class="rr-field__hint">The selected workspace defines which projects are available below.</p>
            </label>

            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">Workspace name</span>
                <input
                  v-model="workspaceForm.name"
                  class="rr-control"
                  type="text"
                  placeholder="Acme knowledge base"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">Workspace slug</span>
                <input
                  v-model="workspaceForm.slug"
                  class="rr-control"
                  type="text"
                  placeholder="acme-kb"
                >
              </label>
            </div>
          </div>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!workspaceForm.name || !workspaceForm.slug"
              @click="createWorkspaceItem"
            >
              Create workspace
            </button>
          </div>

          <p
            v-if="workspaceError"
            class="rr-banner"
            data-tone="danger"
          >
            {{ workspaceError }}
          </p>
        </article>

        <article class="rr-panel setup-panel">
          <div class="setup-panel__heading">
            <div>
              <p class="rr-kicker">Project</p>
              <h3>Create or select a project</h3>
            </div>
            <StatusBadge
              :status="selectedProject ? 'ready' : selectedWorkspace ? 'partial' : 'blocked'"
              :label="selectedProject ? 'Selected' : selectedWorkspace ? 'Workspace ready' : 'Needs workspace'"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">Selected project</span>
              <select
                v-model="selectedProjectId"
                class="rr-control"
                :disabled="projects.length === 0"
              >
                <option value="">Choose project</option>
                <option v-for="project in projects" :key="project.id" :value="project.id">
                  {{ project.name }} ({{ project.slug }})
                </option>
              </select>
              <p class="rr-field__hint">Ingest and Ask will use this project automatically.</p>
            </label>

            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">Project name</span>
                <input
                  v-model="projectForm.name"
                  class="rr-control"
                  type="text"
                  placeholder="Customer support docs"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">Project slug</span>
                <input
                  v-model="projectForm.slug"
                  class="rr-control"
                  type="text"
                  placeholder="support-docs"
                >
              </label>
            </div>

            <label class="rr-field">
              <span class="rr-field__label">Description</span>
              <textarea
                v-model="projectForm.description"
                class="rr-control"
                rows="4"
                placeholder="Optional description"
              />
            </label>
          </div>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!projectForm.name || !projectForm.slug || !getSelectedWorkspaceId()"
              @click="createProjectItem"
            >
              Create project
            </button>
          </div>

          <p
            v-if="projectError"
            class="rr-banner"
            data-tone="danger"
          >
            {{ projectError }}
          </p>
        </article>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.setup-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--rr-space-4);
}

.setup-panel {
  align-content: start;
}

.setup-panel__heading {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.setup-panel__heading h3 {
  margin: 4px 0 0;
}

@media (width <= 1100px) {
  .setup-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 700px) {
  .setup-panel__heading {
    flex-direction: column;
  }
}
</style>
