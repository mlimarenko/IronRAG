<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { createProject, createWorkspace, fetchProjects, fetchWorkspaces } from 'src/boot/api'
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
  set: (value: string) => setSelectedProjectId(value),
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
  <section class="setup-page">
    <header>
      <h2>Setup</h2>
      <p>Create a workspace and project, then keep them selected for ingestion and querying.</p>
    </header>

    <p v-if="successMessage" class="success-banner">{{ successMessage }}</p>

    <div class="setup-grid">
      <article class="panel">
        <h3>1. Workspace</h3>
        <label class="field">
          <span>Selected workspace</span>
          <select v-model="selectedWorkspaceId">
            <option value="">Choose workspace</option>
            <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
              {{ workspace.name }} ({{ workspace.slug }})
            </option>
          </select>
        </label>

        <div class="form-grid">
          <label class="field">
            <span>Workspace name</span>
            <input v-model="workspaceForm.name" type="text" placeholder="Acme knowledge base">
          </label>
          <label class="field">
            <span>Workspace slug</span>
            <input v-model="workspaceForm.slug" type="text" placeholder="acme-kb">
          </label>
        </div>

        <button type="button" :disabled="!workspaceForm.name || !workspaceForm.slug" @click="createWorkspaceItem">
          Create workspace
        </button>
        <p v-if="workspaceError" class="error-banner">{{ workspaceError }}</p>
      </article>

      <article class="panel">
        <h3>2. Project</h3>
        <label class="field">
          <span>Selected project</span>
          <select v-model="selectedProjectId" :disabled="projects.length === 0">
            <option value="">Choose project</option>
            <option v-for="project in projects" :key="project.id" :value="project.id">
              {{ project.name }} ({{ project.slug }})
            </option>
          </select>
        </label>

        <div class="form-grid">
          <label class="field">
            <span>Project name</span>
            <input v-model="projectForm.name" type="text" placeholder="Customer support docs">
          </label>
          <label class="field">
            <span>Project slug</span>
            <input v-model="projectForm.slug" type="text" placeholder="support-docs">
          </label>
        </div>

        <label class="field">
          <span>Description</span>
          <textarea v-model="projectForm.description" rows="3" placeholder="Optional description" />
        </label>

        <button type="button" :disabled="!projectForm.name || !projectForm.slug || !getSelectedWorkspaceId()" @click="createProjectItem">
          Create project
        </button>
        <p v-if="projectError" class="error-banner">{{ projectError }}</p>
      </article>
    </div>
  </section>
</template>

<style scoped>
.setup-page {
  display: grid;
  gap: 16px;
}

.setup-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.panel {
  display: grid;
  gap: 14px;
  padding: 20px;
  border: 1px solid #d7dee7;
  border-radius: 16px;
  background: #f8fbff;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

.field {
  display: grid;
  gap: 6px;
}

input,
textarea,
select {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #c8d5e3;
  border-radius: 10px;
  font: inherit;
  background: #fff;
}

button {
  width: fit-content;
  padding: 10px 16px;
  border: 0;
  border-radius: 999px;
  background: #215dff;
  color: #fff;
  font: inherit;
  font-weight: 600;
  cursor: pointer;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.error-banner,
.success-banner {
  padding: 12px 14px;
  border-radius: 10px;
}

.error-banner {
  background: #fde2e2;
  color: #b42318;
}

.success-banner {
  background: #dcfce7;
  color: #166534;
}

@media (width <= 1100px) {
  .setup-grid,
  .form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
