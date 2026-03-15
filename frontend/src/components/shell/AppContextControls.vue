<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'

import {
  fetchProjects,
  fetchWorkspaces,
  type ProjectSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import { setWorkspaceWithProjectReset } from 'src/lib/flowSelection'
import { getSelectedProjectId, getSelectedWorkspaceId, setSelectedProjectId } from 'src/stores/flow'

const props = withDefaults(
  defineProps<{
    compact?: boolean
  }>(),
  {
    compact: false,
  },
)

const { t } = useI18n()
const route = useRoute()

const loading = ref(false)
const error = ref<string | null>(null)
const workspaces = ref<WorkspaceSummary[]>([])
const projects = ref<ProjectSummary[]>([])

const selectedWorkspaceId = ref('')
const selectedProjectId = ref('')

const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === selectedWorkspaceId.value) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === selectedProjectId.value) ?? null,
)
const hasContext = computed(() => Boolean(selectedWorkspace.value && selectedProject.value))
const hasWorkspaceChoices = computed(() => workspaces.value.length > 1)
const hasProjectChoices = computed(() => projects.value.length > 1)
const isAdvancedRoute = computed(() => route.meta.shellSection === 'advanced')

async function loadProjects(workspaceId: string, preserveProjectId = true) {
  if (!workspaceId) {
    projects.value = []
    selectedProjectId.value = ''
    setSelectedProjectId('')
    return
  }

  const items = await fetchProjects(workspaceId)
  projects.value = items

  const storedProjectId = preserveProjectId ? getSelectedProjectId() : ''
  const matchingStoredProject = items.find((item) => item.id === storedProjectId)
  const matchingCurrentProject = items.find((item) => item.id === selectedProjectId.value)
  let nextProjectId = ''

  if (matchingStoredProject) {
    nextProjectId = matchingStoredProject.id
  } else if (matchingCurrentProject) {
    nextProjectId = matchingCurrentProject.id
  } else if (items[0]) {
    nextProjectId = items[0].id
  }

  selectedProjectId.value = nextProjectId
  setSelectedProjectId(nextProjectId)
}

async function hydrateContext() {
  loading.value = true
  error.value = null

  try {
    const workspaceItems = await fetchWorkspaces()
    workspaces.value = workspaceItems

    const matchingWorkspace = workspaceItems.find((item) => item.id === getSelectedWorkspaceId())
    let nextWorkspaceId = ''

    if (matchingWorkspace) {
      nextWorkspaceId = matchingWorkspace.id
    } else if (workspaceItems[0]) {
      nextWorkspaceId = workspaceItems[0].id
    }

    selectedWorkspaceId.value = nextWorkspaceId

    if (nextWorkspaceId && nextWorkspaceId !== getSelectedWorkspaceId()) {
      setWorkspaceWithProjectReset(nextWorkspaceId)
    }

    await loadProjects(nextWorkspaceId)
  } catch (nextError) {
    error.value = nextError instanceof Error ? nextError.message : t('shell.context.error')
    workspaces.value = []
    projects.value = []
    selectedWorkspaceId.value = ''
    selectedProjectId.value = ''
  } finally {
    loading.value = false
  }
}

async function handleWorkspaceChange(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  selectedWorkspaceId.value = value
  setWorkspaceWithProjectReset(value)
  await loadProjects(value, false)
}

function handleProjectChange(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  selectedProjectId.value = value
  setSelectedProjectId(value)
}

watch(
  () => route.fullPath,
  () => {
    if (!workspaces.value.length || loading.value) {
      return
    }

    const workspaceId = getSelectedWorkspaceId()
    const projectId = getSelectedProjectId()

    if (workspaceId && workspaceId !== selectedWorkspaceId.value) {
      selectedWorkspaceId.value = workspaceId
      void loadProjects(workspaceId)
      return
    }

    if (projectId && projectId !== selectedProjectId.value) {
      selectedProjectId.value = projectId
    }
  },
)

onMounted(() => {
  void hydrateContext()
})
</script>

<template>
  <section class="app-context-controls" :data-compact="props.compact">
    <div class="app-context-controls__header">
      <div>
        <p class="app-context-controls__eyebrow">{{ t('shell.context.eyebrow') }}</p>
        <p class="app-context-controls__summary">
          {{
            loading
              ? t('shell.context.loading')
              : hasContext
                ? t('shell.context.ready', {
                    workspace: selectedWorkspace?.name ?? t('shell.context.none'),
                    library: selectedProject?.name ?? t('shell.context.none'),
                  })
                : t('shell.context.empty')
          }}
        </p>
      </div>

      <RouterLink v-if="isAdvancedRoute" to="/documents" class="app-context-controls__back-link">
        {{ t('shell.context.backToDocuments') }}
      </RouterLink>
    </div>

    <p v-if="error" class="app-context-controls__error">
      {{ error }}
    </p>

    <div class="app-context-controls__fields">
      <label class="app-context-controls__field">
        <span>{{ t('shell.context.workspace') }}</span>
        <select
          class="app-context-controls__select"
          :value="selectedWorkspaceId"
          :disabled="loading || !workspaces.length"
          @change="void handleWorkspaceChange($event)"
        >
          <option value="">{{ t('shell.context.none') }}</option>
          <option v-for="workspace in workspaces" :key="workspace.id" :value="workspace.id">
            {{ workspace.name }}
          </option>
        </select>
        <small v-if="!hasWorkspaceChoices && selectedWorkspace">{{
          t('shell.context.defaultWorkspace')
        }}</small>
      </label>

      <label class="app-context-controls__field">
        <span>{{ t('shell.context.library') }}</span>
        <select
          class="app-context-controls__select"
          :value="selectedProjectId"
          :disabled="loading || !projects.length"
          @change="handleProjectChange"
        >
          <option value="">{{ t('shell.context.none') }}</option>
          <option v-for="project in projects" :key="project.id" :value="project.id">
            {{ project.name }}
          </option>
        </select>
        <small v-if="!hasProjectChoices && selectedProject">{{
          t('shell.context.defaultLibrary')
        }}</small>
      </label>
    </div>

    <details class="app-context-controls__advanced">
      <summary>{{ t('shell.context.advanced') }}</summary>
      <p>{{ t('shell.context.advancedHint') }}</p>
      <RouterLink to="/advanced/context" class="app-context-controls__manage-link">
        {{ t('shell.context.manage') }}
      </RouterLink>
    </details>
  </section>
</template>

<style scoped>
.app-context-controls {
  display: grid;
  gap: 12px;
  padding: 14px;
  border: 1px solid rgb(15 23 42 / 0.08);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.78);
}

.app-context-controls[data-compact='true'] {
  padding: 12px;
  border-radius: calc(var(--rr-radius-md) + 2px);
}

.app-context-controls__header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
}

.app-context-controls__eyebrow {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-context-controls__summary {
  margin: 4px 0 0;
  font-size: 0.86rem;
  color: var(--rr-color-text-secondary);
}

.app-context-controls__back-link,
.app-context-controls__manage-link {
  color: var(--rr-color-accent-700);
  text-decoration: none;
  font-size: 0.85rem;
  font-weight: 650;
}

.app-context-controls__error {
  margin: 0;
  color: var(--rr-color-danger-700, #b42318);
  font-size: 0.84rem;
}

.app-context-controls__fields {
  display: grid;
  gap: 10px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.app-context-controls__field {
  display: grid;
  gap: 6px;
}

.app-context-controls__field span,
.app-context-controls__field small,
.app-context-controls__advanced,
.app-context-controls__advanced p {
  color: var(--rr-color-text-secondary);
}

.app-context-controls__field span {
  font-size: 0.78rem;
  font-weight: 700;
}

.app-context-controls__field small,
.app-context-controls__advanced p {
  font-size: 0.78rem;
}

.app-context-controls__select {
  min-height: 40px;
  padding: 0 12px;
  border: 1px solid rgb(15 23 42 / 0.12);
  border-radius: 12px;
  background: rgb(248 250 252 / 0.92);
  color: var(--rr-color-text-primary);
}

.app-context-controls__advanced summary {
  cursor: pointer;
  font-size: 0.8rem;
  font-weight: 700;
}

.app-context-controls__advanced p {
  margin: 8px 0 10px;
}

@media (width <= 720px) {
  .app-context-controls__fields {
    grid-template-columns: 1fr;
  }

  .app-context-controls__header {
    flex-direction: column;
  }
}
</style>
