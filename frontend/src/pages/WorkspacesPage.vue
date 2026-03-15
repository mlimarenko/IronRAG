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
const progressValue = computed(() => {
  if (selectedProject.value) {
    return 100
  }

  if (selectedWorkspace.value) {
    return 55
  }

  return 20
})
const progressLabel = computed(() => {
  if (selectedProject.value) {
    return t('flow.processing.progress.complete')
  }

  if (selectedWorkspace.value) {
    return t('flow.processing.progress.workspaceReady')
  }

  return t('flow.processing.progress.start')
})
const primarySetupAction = computed(() => {
  if (selectedProject.value) {
    return {
      to: '/files',
      label: t('flow.processing.hero.primaryAction.files'),
      hint: t('flow.processing.progress.complete'),
      muted: false,
    }
  }

  if (selectedWorkspace.value) {
    return {
      to: '#library-form',
      label: t('flow.processing.hero.primaryAction.project'),
      hint: t('flow.processing.progress.workspaceReady'),
      muted: false,
    }
  }

  return {
    to: '#setup-access',
    label: t('flow.processing.hero.primaryAction.access'),
    hint: t('flow.processing.progress.start'),
    muted: true,
  }
})
const setupChecklist = computed(() => [
  {
    key: 'workspace',
    title: t('flow.processing.checklist.workspace.title'),
    description: t('flow.processing.checklist.workspace.description'),
    complete: Boolean(selectedWorkspace.value),
    badge: selectedWorkspace.value
      ? t('flow.processing.checklist.done')
      : t('flow.processing.checklist.todo'),
    status: selectedWorkspace.value ? 'Healthy' : 'Warning',
  },
  {
    key: 'project',
    title: t('flow.processing.checklist.project.title'),
    description: t('flow.processing.checklist.project.description'),
    complete: Boolean(selectedProject.value),
    badge: selectedProject.value
      ? t('flow.processing.checklist.done')
      : selectedWorkspace.value
        ? t('flow.processing.checklist.inProgress')
        : t('flow.processing.checklist.blocked'),
    status: selectedProject.value ? 'Healthy' : selectedWorkspace.value ? 'Warning' : 'Blocked',
  },
  {
    key: 'files',
    title: t('flow.processing.checklist.files.title'),
    description: t('flow.processing.checklist.files.description'),
    complete: Boolean(selectedProject.value),
    badge: selectedProject.value
      ? t('flow.processing.checklist.readyNext')
      : t('flow.processing.checklist.waiting'),
    status: selectedProject.value ? 'Healthy' : 'Info',
  },
])

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
        <RouterLink class="rr-button rr-button--secondary" to="/files" :aria-disabled="!selectedProject">
          {{ t('flow.processing.stats.nextReady') }}
        </RouterLink>
      </template>

      <article class="setup-hero">
        <div class="setup-hero__copy">
          <p class="rr-kicker">{{ t('flow.processing.hero.eyebrow') }}</p>
          <h2>{{ t('flow.processing.hero.title') }}</h2>
          <p>{{ t('flow.processing.hero.description') }}</p>

          <div class="setup-hero__selection">
            <article class="setup-hero__selection-item">
              <span>{{ t('flow.processing.hero.cards.workspace.title') }}</span>
              <strong>{{ selectedWorkspace?.name ?? t('flow.processing.hero.cards.workspace.empty') }}</strong>
            </article>
            <article class="setup-hero__selection-item">
              <span>{{ t('flow.processing.hero.cards.project.title') }}</span>
              <strong>{{ selectedProject?.name ?? t('flow.processing.hero.cards.project.empty') }}</strong>
            </article>
          </div>
        </div>

        <div class="setup-hero__progress">
          <div class="setup-hero__progress-top">
            <span>{{ t('flow.processing.hero.progressLabel') }}</span>
            <strong>{{ progressValue }}%</strong>
          </div>
          <div class="setup-hero__progress-bar" aria-hidden="true">
            <span :style="{ width: `${progressValue}%` }" />
          </div>
          <p>{{ progressLabel }}</p>

          <RouterLink
            v-if="primarySetupAction.to.startsWith('/')"
            class="rr-button setup-hero__action"
            :class="{ 'rr-button--secondary': primarySetupAction.muted }"
            :to="primarySetupAction.to"
          >
            {{ primarySetupAction.label }}
          </RouterLink>
          <a
            v-else
            class="rr-button setup-hero__action"
            :class="{ 'rr-button--secondary': primarySetupAction.muted }"
            :href="primarySetupAction.to"
          >
            {{ primarySetupAction.label }}
          </a>
          <small class="setup-hero__action-hint">{{ primarySetupAction.hint }}</small>
        </div>
      </article>

      <p v-if="successMessage" class="rr-banner" data-tone="success">
        {{ successMessage }}
      </p>

      <div class="setup-layout">
        <div class="setup-layout__main">
          <div id="setup-access">
            <AuthSessionPanel
              :title="t('flow.processing.auth.title')"
              :description="t('flow.processing.auth.description')"
              :context-note="t('flow.processing.auth.note')"
              @updated="void loadSetupState()"
            />
          </div>

          <div class="setup-grid">
            <article class="rr-panel rr-panel--accent setup-panel">
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

                <div class="rr-form-grid rr-form-grid--two">
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
              </div>

              <div class="rr-action-row">
                <button
                  type="button"
                  class="rr-button"
                  :disabled="!workspaceForm.name || !workspaceForm.slug"
                  @click="createWorkspaceItem"
                >
                  {{ t('flow.processing.panels.workspace.create') }}
                </button>
              </div>

              <p v-if="workspaceError" class="rr-banner" data-tone="danger">
                {{ workspaceError }}
              </p>
            </article>

            <article id="library-form" class="rr-panel setup-panel">
              <div class="setup-panel__heading">
                <div>
                  <p class="rr-kicker">{{ t('flow.processing.panels.project.kicker') }}</p>
                  <h3>{{ t('flow.processing.panels.project.title') }}</h3>
                  <p>{{ t('flow.processing.panels.project.helper') }}</p>
                </div>
                <StatusBadge
                  :status="selectedProject ? 'ready' : selectedWorkspace ? 'partial' : 'blocked'"
                  :label="
                    selectedProject
                      ? t('flow.processing.panels.project.selectedBadge')
                      : selectedWorkspace
                        ? t('flow.processing.panels.project.workspaceReady')
                        : t('flow.processing.panels.project.needsWorkspace')
                  "
                />
              </div>

              <div class="rr-form-grid">
                <label class="rr-field">
                  <span class="rr-field__label">{{ t('flow.processing.panels.project.selected') }}</span>
                  <select v-model="selectedProjectId" class="rr-control" :disabled="projects.length === 0">
                    <option value="">{{ t('flow.processing.panels.project.empty') }}</option>
                    <option v-for="project in projects" :key="project.id" :value="project.id">
                      {{ project.name }} ({{ project.slug }})
                    </option>
                  </select>
                </label>

                <div class="rr-form-grid rr-form-grid--two">
                  <label class="rr-field">
                    <span class="rr-field__label">{{ t('flow.processing.panels.project.name') }}</span>
                    <input
                      v-model="projectForm.name"
                      class="rr-control"
                      type="text"
                      :placeholder="t('flow.processing.placeholders.projectName')"
                    >
                  </label>
                  <label class="rr-field">
                    <span class="rr-field__label">{{ t('flow.processing.panels.project.slug') }}</span>
                    <input
                      v-model="projectForm.slug"
                      class="rr-control"
                      type="text"
                      :placeholder="t('flow.processing.placeholders.projectSlug')"
                    >
                  </label>
                </div>

                <label class="rr-field">
                  <span class="rr-field__label">{{ t('flow.processing.panels.project.description') }}</span>
                  <textarea
                    v-model="projectForm.description"
                    class="rr-control"
                    rows="3"
                    :placeholder="t('flow.processing.panels.project.descriptionPlaceholder')"
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
                  {{ t('flow.processing.panels.project.create') }}
                </button>
              </div>

              <p v-if="projectError" class="rr-banner" data-tone="danger">
                {{ projectError }}
              </p>
            </article>
          </div>
        </div>

        <aside class="setup-sidebar">
          <details class="rr-panel setup-sidebar__panel" :open="!selectedProject">
            <summary class="setup-sidebar__summary">
              <div>
                <p class="rr-kicker">{{ t('flow.processing.checklist.eyebrow') }}</p>
                <h3>{{ t('flow.processing.checklist.title') }}</h3>
              </div>
              <StatusBadge :status="setupStatus.status" :label="setupStatus.label" />
            </summary>

            <div class="setup-checklist">
              <article
                v-for="item in setupChecklist"
                :key="item.key"
                class="setup-checklist__item"
                :data-complete="item.complete"
              >
                <div>
                  <h4>{{ item.title }}</h4>
                  <p>{{ item.description }}</p>
                </div>
                <StatusBadge :status="item.status" :label="item.badge" />
              </article>
            </div>
          </details>
        </aside>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.setup-page {
  gap: var(--rr-space-5);
}

.setup-hero {
  display: grid;
  grid-template-columns: minmax(0, 1.5fr) minmax(260px, 0.9fr);
  gap: var(--rr-space-4);
  padding: clamp(var(--rr-space-4), 3vw, var(--rr-space-6));
  border-radius: var(--rr-radius-xl);
  background:
    linear-gradient(180deg, rgb(255 255 255 / 0.96), rgb(234 240 255 / 0.7)),
    var(--rr-color-bg-surface-strong);
  border: 1px solid rgb(29 78 216 / 0.12);
  box-shadow: var(--rr-shadow-sm);
}

.setup-hero__copy,
.setup-hero__progress,
.setup-sidebar__panel,
.setup-checklist,
.setup-checklist__item,
.setup-hero__selection,
.setup-hero__selection-item {
  display: grid;
  gap: var(--rr-space-3);
}

.setup-hero__copy h2,
.setup-hero__selection-item strong,
.setup-panel__heading h3,
.setup-sidebar__summary h3,
.setup-checklist__item h4 {
  margin: 0;
}

.setup-hero__copy p,
.setup-hero__progress p,
.setup-panel__heading p,
.setup-checklist__item p,
.setup-sidebar__panel p,
.setup-hero__selection-item span,
.setup-hero__action-hint {
  margin: 0;
}

.setup-hero__progress {
  align-content: start;
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.78);
  border: 1px solid var(--rr-border-default);
}

.setup-hero__progress-top {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
}

.setup-hero__progress-bar {
  height: 0.6rem;
  border-radius: var(--rr-radius-pill);
  overflow: hidden;
  background: rgb(148 163 184 / 0.18);
}

.setup-hero__progress-bar span {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: linear-gradient(90deg, var(--rr-color-accent-600), #60a5fa);
}

.setup-hero__selection {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.setup-hero__selection-item {
  min-width: 0;
  padding: var(--rr-space-3);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.72);
  border: 1px solid var(--rr-border-default);
}

.setup-hero__selection-item span {
  font-size: 0.8rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.setup-hero__action {
  width: 100%;
  justify-content: center;
}

.setup-hero__action-hint {
  display: block;
  color: var(--rr-color-text-secondary);
}

.setup-layout {
  display: grid;
  grid-template-columns: minmax(0, 1.6fr) minmax(280px, 0.8fr);
  gap: var(--rr-space-4);
  align-items: start;
}

.setup-layout__main,
.setup-sidebar {
  display: grid;
  gap: var(--rr-space-4);
}

.setup-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--rr-space-4);
}

.setup-panel__heading,
.setup-sidebar__summary,
.setup-checklist__item {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.setup-panel__heading p,
.setup-checklist__item p {
  color: var(--rr-color-text-secondary);
}

.setup-checklist__item {
  padding: var(--rr-space-3);
  border-radius: var(--rr-radius-md);
  background: var(--rr-surface-panel-muted);
  border: 1px solid transparent;
}

.setup-checklist__item[data-complete='true'] {
  border-color: rgb(34 197 94 / 0.2);
  background: rgb(234 248 239 / 0.72);
}

.setup-sidebar__summary {
  cursor: pointer;
  list-style: none;
}

.setup-sidebar__summary::-webkit-details-marker {
  display: none;
}

.setup-sidebar__summary + .setup-checklist {
  margin-top: var(--rr-space-3);
}

@media (width <= 1180px) {
  .setup-layout {
    grid-template-columns: 1fr;
  }

  .setup-sidebar {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (width <= 980px) {
  .setup-hero,
  .setup-hero__selection,
  .setup-grid,
  .setup-sidebar {
    grid-template-columns: 1fr;
  }
}

@media (width <= 700px) {
  .setup-page {
    gap: var(--rr-space-4);
  }

  .setup-hero {
    padding: var(--rr-space-4);
  }

  .setup-panel__heading,
  .setup-sidebar__summary,
  .setup-checklist__item {
    flex-direction: column;
  }
}
</style>
