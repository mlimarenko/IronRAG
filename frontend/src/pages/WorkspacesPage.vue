<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  createProject,
  createWorkspace,
  fetchProjects,
  fetchWorkspaces,
  isUnauthorizedApiError,
} from 'src/boot/api'
import AuthSessionPanel from 'src/components/shell/AuthSessionPanel.vue'
import CrossSurfaceGuide from 'src/components/shell/CrossSurfaceGuide.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import ProductSpine from 'src/components/shell/ProductSpine.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
} from 'src/stores/flow'
import { setWorkspaceWithProjectReset, syncWorkspaceProjectScope } from 'src/lib/flowSelection'

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

async function loadSetupState() {
  workspaces.value = await fetchWorkspaces()
  const workspaceId = getSelectedWorkspaceId() || workspaces.value[0]?.id || ''

  if (workspaceId) {
    if (workspaceId !== getSelectedWorkspaceId()) {
      setWorkspaceWithProjectReset(workspaceId)
    }
    projects.value = await fetchProjects(workspaceId)
  } else {
    projects.value = []
  }

  syncWorkspaceProjectScope(workspaces.value, projects.value)
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
      slug: workspaceForm.value.slug.trim(),
      name: workspaceForm.value.name.trim(),
    })
    workspaces.value = [created, ...workspaces.value.filter((item) => item.id !== created.id)]
    workspaceForm.value = { slug: '', name: '' }
    setWorkspaceWithProjectReset(created.id)
    successMessage.value = `${t('flow.processing.success')}: ${created.name}`
  } catch (error) {
    workspaceError.value = formatProtectedCreateError(error, t('flow.processing.errors.createWorkspace'))
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
      slug: projectForm.value.slug.trim(),
      name: projectForm.value.name.trim(),
      description: projectForm.value.description.trim() || null,
    })
    projects.value = [created, ...projects.value.filter((item) => item.id !== created.id)]
    projectForm.value = { slug: '', name: '', description: '' }
    syncWorkspaceProjectScope(workspaces.value, projects.value)
    successMessage.value = `${t('flow.processing.success')}: ${created.name}`
  } catch (error) {
    projectError.value = formatProtectedCreateError(error, t('flow.processing.errors.createProject'))
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
          {{ t('flow.processing.stats.nextReady') }}
        </RouterLink>
      </template>

      <article class="setup-reset rr-panel rr-panel--accent">
        <div class="setup-reset__hero">
          <div class="setup-reset__copy">
            <p class="rr-kicker">{{ t('flow.processing.hero.eyebrow') }}</p>
            <h2>{{ t('flow.processing.hero.title') }}</h2>
            <p>{{ t('flow.processing.hero.description') }}</p>
          </div>
          <StatusBadge :status="setupStatus.status" :label="setupStatus.label" emphasis="strong" />
        </div>

        <p v-if="successMessage" class="rr-banner" data-tone="success">
          {{ successMessage }}
        </p>

        <div class="setup-reset__scope">
          <article class="setup-reset__scope-card">
            <span>{{ t('flow.processing.hero.cards.workspace.title') }}</span>
            <strong>{{ selectedWorkspace?.name ?? t('flow.processing.hero.cards.workspace.empty') }}</strong>
          </article>
          <article class="setup-reset__scope-card">
            <span>{{ t('flow.processing.hero.cards.project.title') }}</span>
            <strong>{{ selectedProject?.name ?? t('flow.processing.hero.cards.project.empty') }}</strong>
          </article>
        </div>

        <AuthSessionPanel
          :title="t('flow.processing.auth.title')"
          :description="t('flow.processing.auth.description')"
          :context-note="t('flow.processing.auth.note')"
          @updated="void loadSetupState()"
        />

        <div class="setup-reset__forms">
          <article class="rr-panel setup-panel">
            <div class="setup-panel__heading">
              <div>
                <p class="rr-kicker">{{ t('flow.processing.panels.workspace.kicker') }}</p>
                <h3>{{ t('flow.processing.panels.workspace.title') }}</h3>
                <p>{{ t('flow.processing.panels.workspace.helper') }}</p>
              </div>
              <StatusBadge
                :status="selectedWorkspace ? 'ready' : 'draft'"
                :label="selectedWorkspace ? t('flow.processing.panels.workspace.selectedBadge') : t('flow.processing.panels.workspace.required')"
              />
            </div>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.workspace.selected') }}</span>
                <select v-model="selectedWorkspaceId" class="rr-control">
                  <option value="">{{ t('flow.processing.panels.workspace.empty') }}</option>
                  <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
                    {{ workspace.name }} ({{ workspace.slug }})
                  </option>
                </select>
              </label>

              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.workspace.name') }}</span>
                <input
                  v-model="workspaceForm.name"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.processing.placeholders.workspaceName')"
                >
              </label>

              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.workspace.slug') }}</span>
                <input
                  v-model="workspaceForm.slug"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.processing.placeholders.workspaceSlug')"
                >
              </label>
            </div>

            <p v-if="workspaceError" class="rr-banner" data-tone="danger">{{ workspaceError }}</p>

            <div class="rr-action-row">
              <button type="button" class="rr-button" @click="createWorkspaceItem">
                {{ t('flow.processing.panels.workspace.create') }}
              </button>
            </div>
          </article>

          <article class="rr-panel setup-panel">
            <div class="setup-panel__heading">
              <div>
                <p class="rr-kicker">{{ t('flow.processing.panels.project.kicker') }}</p>
                <h3>{{ t('flow.processing.panels.project.title') }}</h3>
                <p>{{ t('flow.processing.panels.project.helper') }}</p>
              </div>
              <StatusBadge
                :status="selectedProject ? 'ready' : selectedWorkspace ? 'partial' : 'blocked'"
                :label="selectedProject ? t('flow.processing.panels.project.selectedBadge') : selectedWorkspace ? t('flow.processing.panels.project.workspaceReady') : t('flow.processing.panels.project.needsWorkspace')"
              />
            </div>

            <div class="rr-form-grid">
              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.project.selected') }}</span>
                <select v-model="selectedProjectId" class="rr-control" :disabled="!selectedWorkspaceId">
                  <option value="">{{ t('flow.processing.panels.project.empty') }}</option>
                  <option v-for="project in projects" :key="project.id" :value="project.id">
                    {{ project.name }} ({{ project.slug }})
                  </option>
                </select>
              </label>

              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.project.name') }}</span>
                <input
                  v-model="projectForm.name"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.processing.placeholders.projectName')"
                  :disabled="!selectedWorkspaceId"
                >
              </label>

              <label class="rr-field">
                <span class="rr-field__label">{{ t('flow.processing.panels.project.slug') }}</span>
                <input
                  v-model="projectForm.slug"
                  class="rr-control"
                  type="text"
                  :placeholder="t('flow.processing.placeholders.projectSlug')"
                  :disabled="!selectedWorkspaceId"
                >
              </label>

              <label class="rr-field rr-field--full">
                <span class="rr-field__label">{{ t('flow.processing.panels.project.description') }}</span>
                <textarea
                  v-model="projectForm.description"
                  class="rr-control"
                  rows="4"
                  :placeholder="t('flow.processing.panels.project.descriptionPlaceholder')"
                  :disabled="!selectedWorkspaceId"
                />
              </label>
            </div>

            <p v-if="projectError" class="rr-banner" data-tone="danger">{{ projectError }}</p>

            <div class="rr-action-row">
              <button type="button" class="rr-button" :disabled="!selectedWorkspaceId" @click="createProjectItem">
                {{ t('flow.processing.panels.project.create') }}
              </button>
              <RouterLink class="rr-button rr-button--secondary" to="/files" :aria-disabled="!selectedProject">
                {{ t('flow.processing.hero.primaryAction.files') }}
              </RouterLink>
            </div>
          </article>
        </div>
      </article>

      <CrossSurfaceGuide active-section="processing" />
      <ProductSpine active-section="processing" />
    </PageSection>
  </section>
</template>

<style scoped>
.setup-page {
  gap: 1.5rem;
}

.setup-reset,
.setup-reset__scope-card,
.setup-panel {
  display: grid;
  gap: 1rem;
}

.setup-reset__hero,
.setup-panel__heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
}

.setup-reset__hero h2,
.setup-panel__heading h3,
.setup-reset__scope-card strong {
  margin: 0;
  color: var(--rr-ink-strong);
}

.setup-reset__hero p,
.setup-panel__heading p,
.setup-reset__scope-card span {
  margin: 0;
  color: var(--rr-ink-muted);
}

.setup-reset__scope,
.setup-reset__forms {
  display: grid;
  gap: 1rem;
}

.setup-reset__scope {
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
}

.setup-reset__scope-card {
  padding: 1rem;
  border-radius: 1rem;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.08);
}

@media (max-width: 720px) {
  .setup-reset__hero,
  .setup-panel__heading {
    flex-direction: column;
  }
}
</style>
