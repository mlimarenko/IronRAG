<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { createProject, createWorkspace, fetchProjects, isUnauthorizedApiError } from 'src/boot/api'
import AuthSessionPanel from 'src/components/shell/AuthSessionPanel.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import { getSelectedProjectId, getSelectedWorkspaceId, setSelectedProjectId } from 'src/stores/flow'
import { setWorkspaceWithProjectReset, syncWorkspaceProjectScope } from 'src/lib/flowSelection'
import { hydrateWorkspaceProjectScope } from 'src/lib/productFlow'

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
const workspaceForm = ref({ name: '', slug: '' })
const projectForm = ref({ name: '', slug: '', description: '' })
const showWorkspaceAdvanced = ref(false)
const showProjectAdvanced = ref(false)
const workspaceError = ref<string | null>(null)
const projectError = ref<string | null>(null)
const successMessage = ref<string | null>(null)

const selectedWorkspaceId = computed({
  get: () => getSelectedWorkspaceId(),
  set: (value: string) => {
    setWorkspaceWithProjectReset(value)
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
    return { status: 'ready', label: t('flow.processing.statusReady') }
  }

  if (selectedWorkspace.value) {
    return { status: 'partial', label: t('flow.processing.statusPartial') }
  }

  return { status: 'draft', label: t('flow.processing.statusDraft') }
})

watch(selectedWorkspaceId, async (value) => {
  projects.value = value ? await fetchProjects(value) : []
  syncWorkspaceProjectScope(workspaces.value, projects.value)
})

function slugify(value: string): string {
  return value
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

function updateWorkspaceSlugFromName() {
  if (!showWorkspaceAdvanced.value) {
    workspaceForm.value.slug = slugify(workspaceForm.value.name)
  }
}

function updateProjectSlugFromName() {
  if (!showProjectAdvanced.value) {
    projectForm.value.slug = slugify(projectForm.value.name)
  }
}

const showAuthPanel = computed(() => !workspaces.value.length && !selectedWorkspace.value)
const canCreateWorkspace = computed(() => workspaceForm.value.name.trim().length > 0)
const canCreateProject = computed(
  () => !!selectedWorkspaceId.value && projectForm.value.name.trim().length > 0,
)

async function loadSetupState() {
  await hydrateWorkspaceProjectScope({
    setWorkspaces: (items) => {
      workspaces.value = items
    },
    setProjects: (items) => {
      projects.value = items
    },
  })
}

function formatProtectedCreateError(error: unknown, fallback: string) {
  if (isUnauthorizedApiError(error)) {
    return t('flow.processing.auth.createRequired')
  }

  return error instanceof Error ? error.message : fallback
}

onMounted(async () => {
  await loadSetupState()
})

async function createWorkspaceItem() {
  workspaceError.value = null
  successMessage.value = null
  try {
    const created = await createWorkspace({
      slug: slugify(workspaceForm.value.slug || workspaceForm.value.name),
      name: workspaceForm.value.name.trim(),
    })
    workspaces.value = [created, ...workspaces.value.filter((item) => item.id !== created.id)]
    workspaceForm.value = { name: '', slug: '' }
    showWorkspaceAdvanced.value = false
    setWorkspaceWithProjectReset(created.id)
    successMessage.value = `${t('flow.processing.success')}: ${created.name}`
  } catch (error) {
    workspaceError.value = formatProtectedCreateError(
      error,
      t('flow.processing.errors.createWorkspace'),
    )
  }
}

async function createProjectItem() {
  projectError.value = null
  successMessage.value = null
  if (!getSelectedWorkspaceId()) {
    projectError.value = t('flow.processing.errors.selectWorkspaceFirst')
    return
  }

  try {
    const created = await createProject({
      workspace_id: getSelectedWorkspaceId(),
      slug: slugify(projectForm.value.slug || projectForm.value.name),
      name: projectForm.value.name.trim(),
      description: projectForm.value.description.trim() || null,
    })
    projects.value = [created, ...projects.value.filter((item) => item.id !== created.id)]
    projectForm.value = { name: '', slug: '', description: '' }
    showProjectAdvanced.value = false
    syncWorkspaceProjectScope(workspaces.value, projects.value)
    successMessage.value = `${t('flow.processing.success')}: ${created.name}`
  } catch (error) {
    projectError.value = formatProtectedCreateError(
      error,
      t('flow.processing.errors.createProject'),
    )
  }
}
</script>

<template>
  <section class="rr-page-grid setup-page">
    <PageSection
      :eyebrow="t('flow.processing.eyebrow')"
      :title="t('flow.processing.title')"
      :description="t('flow.processing.description')"
      :status="setupStatus.status"
      :status-label="setupStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button" to="/files" :aria-disabled="!selectedProject">
          {{ t('flow.processing.cta.continue') }}
        </RouterLink>
      </template>

      <article class="rr-panel rr-panel--accent setup-flow">
        <div class="setup-flow__header">
          <div>
            <h2>{{ t('flow.processing.hero.title') }}</h2>
            <p>{{ t('flow.processing.hero.description') }}</p>
          </div>
          <RouterLink v-if="selectedProject" class="rr-button" to="/files">
            {{ t('flow.processing.cta.continue') }}
          </RouterLink>
        </div>

        <p v-if="successMessage" class="rr-banner" data-tone="success">
          {{ successMessage }}
        </p>

        <AuthSessionPanel
          v-if="showAuthPanel"
          :title="t('flow.processing.auth.title')"
          :description="t('flow.processing.auth.description')"
          :context-note="t('flow.processing.auth.note')"
          @updated="void loadSetupState()"
        />

        <article class="rr-panel setup-card">
          <div class="setup-card__header">
            <div>
              <p class="rr-kicker">{{ t('flow.processing.panels.workspace.kicker') }}</p>
              <h3>{{ t('flow.processing.panels.workspace.title') }}</h3>
              <p>{{ t('flow.processing.panels.workspace.helper') }}</p>
            </div>
          </div>

          <label class="rr-field">
            <span class="rr-field__label">{{
              t('flow.processing.panels.workspace.selected')
            }}</span>
            <select v-model="selectedWorkspaceId" class="rr-control">
              <option value="">{{ t('flow.processing.panels.workspace.empty') }}</option>
              <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
                {{ workspace.name }}
              </option>
            </select>
          </label>

          <div class="setup-card__divider">{{ t('flow.processing.common.or') }}</div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.processing.panels.workspace.name') }}</span>
              <input
                v-model="workspaceForm.name"
                class="rr-control"
                type="text"
                :placeholder="t('flow.processing.placeholders.workspaceName')"
                @input="updateWorkspaceSlugFromName"
              />
            </label>

            <button
              type="button"
              class="rr-button rr-button--secondary setup-card__advanced-toggle"
              @click="showWorkspaceAdvanced = !showWorkspaceAdvanced"
            >
              {{
                showWorkspaceAdvanced
                  ? t('flow.processing.advanced.hide')
                  : t('flow.processing.advanced.show')
              }}
            </button>

            <label v-if="showWorkspaceAdvanced" class="rr-field">
              <span class="rr-field__label">{{ t('flow.processing.panels.workspace.slug') }}</span>
              <input
                v-model="workspaceForm.slug"
                class="rr-control"
                type="text"
                :placeholder="t('flow.processing.placeholders.workspaceSlug')"
              />
            </label>
          </div>

          <p v-if="workspaceError" class="rr-banner" data-tone="danger">{{ workspaceError }}</p>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!canCreateWorkspace"
              @click="createWorkspaceItem"
            >
              {{ t('flow.processing.panels.workspace.create') }}
            </button>
          </div>
        </article>

        <article class="rr-panel setup-card">
          <div class="setup-card__header">
            <div>
              <p class="rr-kicker">{{ t('flow.processing.panels.project.kicker') }}</p>
              <h3>{{ t('flow.processing.panels.project.title') }}</h3>
              <p>{{ t('flow.processing.panels.project.helper') }}</p>
            </div>
          </div>

          <label class="rr-field">
            <span class="rr-field__label">{{ t('flow.processing.panels.project.selected') }}</span>
            <select v-model="selectedProjectId" class="rr-control" :disabled="!selectedWorkspaceId">
              <option value="">{{ t('flow.processing.panels.project.empty') }}</option>
              <option v-for="project in projects" :key="project.id" :value="project.id">
                {{ project.name }}
              </option>
            </select>
          </label>

          <div class="setup-card__divider">{{ t('flow.processing.common.or') }}</div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('flow.processing.panels.project.name') }}</span>
              <input
                v-model="projectForm.name"
                class="rr-control"
                type="text"
                :placeholder="t('flow.processing.placeholders.projectName')"
                :disabled="!selectedWorkspaceId"
                @input="updateProjectSlugFromName"
              />
            </label>

            <button
              type="button"
              class="rr-button rr-button--secondary setup-card__advanced-toggle"
              :disabled="!selectedWorkspaceId"
              @click="showProjectAdvanced = !showProjectAdvanced"
            >
              {{
                showProjectAdvanced
                  ? t('flow.processing.advanced.hide')
                  : t('flow.processing.advanced.show')
              }}
            </button>

            <template v-if="showProjectAdvanced">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.project.slug') }}</span>
                <input
                  v-model="projectForm.slug"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.processing.placeholders.projectSlug')"
                  :disabled="!selectedWorkspaceId"
                />
              </label>

              <label class="rr-field rr-field--full">
                <span class="rr-field__label">{{
                  t('flow.processing.panels.project.description')
                }}</span>
                <textarea
                  v-model="projectForm.description"
                  class="rr-control"
                  rows="3"
                  :placeholder="t('flow.processing.panels.project.descriptionPlaceholder')"
                  :disabled="!selectedWorkspaceId"
                />
              </label>
            </template>
          </div>

          <p v-if="projectError" class="rr-banner" data-tone="danger">{{ projectError }}</p>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!canCreateProject"
              @click="createProjectItem"
            >
              {{ t('flow.processing.panels.project.create') }}
            </button>
            <RouterLink
              class="rr-button rr-button--secondary"
              to="/files"
              :aria-disabled="!selectedProject"
            >
              {{ t('flow.processing.cta.continue') }}
            </RouterLink>
          </div>
        </article>
      </article>
    </PageSection>
  </section>
</template>

<style scoped>
.setup-page {
  gap: 1.5rem;
}

.setup-flow,
.setup-card {
  display: grid;
  gap: 1rem;
}

.setup-flow__header,
.setup-card__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
}

.setup-flow__header h2,
.setup-card__header h3 {
  margin: 0;
  color: var(--rr-ink-strong);
}

.setup-flow__header p,
.setup-card__header p {
  margin: 0;
  color: var(--rr-ink-muted);
}

.setup-card__divider {
  text-align: center;
  color: var(--rr-ink-muted);
  font-size: 0.875rem;
}

.setup-card__advanced-toggle {
  justify-self: start;
}

@media (max-width: 720px) {
  .setup-flow__header,
  .setup-card__header {
    flex-direction: column;
  }
}
</style>
