<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { createProject, createWorkspace, fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  setSelectedWorkspaceId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
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

const { t } = useI18n()

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
    return { status: 'ready', label: t('flow.workspace.statusReady') }
  }

  if (selectedWorkspace.value) {
    return { status: 'partial', label: t('flow.workspace.statusPartial') }
  }

  return { status: 'draft', label: t('flow.workspace.statusDraft') }
})

watch(selectedWorkspaceId, async (value) => {
  projects.value = value ? await fetchProjects(value) : []
  syncSelectedProjectId(projects.value)
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = syncSelectedWorkspaceId(workspaces.value)
  if (workspaceId) {
    projects.value = await fetchProjects(workspaceId)
    syncSelectedProjectId(projects.value)
  } else {
    projects.value = []
    syncSelectedProjectId([])
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
    successMessage.value = `${t('flow.workspace.success')}: ${created.name}`
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
    syncSelectedProjectId([created, ...projects.value.filter((item) => item.id !== created.id)])
    successMessage.value = `${t('flow.workspace.success')}: ${created.name}`
  } catch (error) {
    projectError.value = error instanceof Error ? error.message : 'Failed to create project'
  }
}
</script>

<template>
  <section class="rr-page-grid setup-page">
    <PageSection
      :eyebrow="t('flow.workspace.eyebrow')"
      :title="t('flow.workspace.title')"
      :description="t('flow.workspace.description')"
      :status="setupStatus.status"
      :status-label="setupStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/ingest" :aria-disabled="!selectedProject">
          {{ t('flow.workspace.stats.nextReady') }}
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.workspace.stats.workspaces') }}</p>
          <strong>{{ workspaces.length }}</strong>
          <p>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.workspace.stats.projects') }}</p>
          <strong>{{ projects.length }}</strong>
          <p>{{ selectedProject?.name ?? t('flow.common.empty') }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.workspace.stats.next') }}</p>
          <strong>{{ selectedProject ? t('flow.workspace.stats.nextReady') : t('flow.workspace.stats.nextSetup') }}</strong>
        </article>
      </div>

      <p v-if="successMessage" class="rr-banner" data-tone="success">
        {{ successMessage }}
      </p>

      <div class="setup-grid">
        <article class="rr-panel rr-panel--accent setup-panel">
          <div class="setup-panel__heading">
            <div>
              <p class="rr-kicker">{{ t('flow.workspace.panels.workspace.kicker') }}</p>
              <h3>{{ t('flow.workspace.panels.workspace.title') }}</h3>
            </div>
            <StatusBadge
              :status="selectedWorkspace ? 'ready' : 'draft'"
              :label="selectedWorkspace ? t('flow.workspace.panels.workspace.selectedBadge') : t('flow.workspace.panels.workspace.required')"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.workspace.panels.workspace.selected') }}</span>
              <select v-model="selectedWorkspaceId" class="rr-control">
                <option value="">{{ t('flow.workspace.panels.workspace.empty') }}</option>
                <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
                  {{ workspace.name }} ({{ workspace.slug }})
                </option>
              </select>
            </label>

            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.workspace.panels.workspace.name') }}</span>
                <input
                  v-model="workspaceForm.name"
                  class="rr-control"
                  type="text"
                  placeholder="Acme knowledge base"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.workspace.panels.workspace.slug') }}</span>
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
              {{ t('flow.workspace.panels.workspace.create') }}
            </button>
          </div>

          <p v-if="workspaceError" class="rr-banner" data-tone="danger">
            {{ workspaceError }}
          </p>
        </article>

        <article class="rr-panel setup-panel">
          <div class="setup-panel__heading">
            <div>
              <p class="rr-kicker">{{ t('flow.workspace.panels.project.kicker') }}</p>
              <h3>{{ t('flow.workspace.panels.project.title') }}</h3>
            </div>
            <StatusBadge
              :status="selectedProject ? 'ready' : selectedWorkspace ? 'partial' : 'blocked'"
              :label="
                selectedProject
                  ? t('flow.workspace.panels.project.selectedBadge')
                  : selectedWorkspace
                    ? t('flow.workspace.panels.project.workspaceReady')
                    : t('flow.workspace.panels.project.needsWorkspace')
              "
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.workspace.panels.project.selected') }}</span>
              <select v-model="selectedProjectId" class="rr-control" :disabled="projects.length === 0">
                <option value="">{{ t('flow.workspace.panels.project.empty') }}</option>
                <option v-for="project in projects" :key="project.id" :value="project.id">
                  {{ project.name }} ({{ project.slug }})
                </option>
              </select>
            </label>

            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.workspace.panels.project.name') }}</span>
                <input
                  v-model="projectForm.name"
                  class="rr-control"
                  type="text"
                  placeholder="Customer support docs"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.workspace.panels.project.slug') }}</span>
                <input
                  v-model="projectForm.slug"
                  class="rr-control"
                  type="text"
                  placeholder="support-docs"
                >
              </label>
            </div>

            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.workspace.panels.project.description') }}</span>
              <textarea
                v-model="projectForm.description"
                class="rr-control"
                rows="4"
                :placeholder="t('flow.workspace.panels.project.descriptionPlaceholder')"
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
              {{ t('flow.workspace.panels.project.create') }}
            </button>
          </div>

          <p v-if="projectError" class="rr-banner" data-tone="danger">
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
