<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'

import {
  buildContextControlsPresentation,
  hydrateContextControlsState,
  switchContextWorkspace,
  type ContextControlsPresentation,
  type ContextControlsState,
} from 'src/lib/contextControls'
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
const state = ref<ContextControlsState>({
  workspaces: [],
  projects: [],
  selectedWorkspaceId: '',
  selectedProjectId: '',
})

const presentation = computed<ContextControlsPresentation>(() =>
  buildContextControlsPresentation(state.value),
)
const isAdvancedRoute = computed(() => route.meta.shellSection === 'advanced')
const selectedWorkspace = computed(() => presentation.value.selectedWorkspace)
const selectedProject = computed(() => presentation.value.selectedProject)
const summaryCopy = computed(() => {
  if (loading.value) {
    return t('shell.context.loading')
  }

  if (error.value) {
    return t('shell.context.errorSummary')
  }

  if (presentation.value.status === 'workspace_only' && selectedWorkspace.value) {
    return t('shell.context.workspaceOnly', { workspace: selectedWorkspace.value.name })
  }

  if (presentation.value.hasContext) {
    return t('shell.context.ready', {
      workspace: selectedWorkspace.value?.name ?? t('shell.context.none'),
      library: selectedProject.value?.name ?? t('shell.context.none'),
    })
  }

  return t('shell.context.empty')
})

async function hydrateContext() {
  loading.value = true
  error.value = null

  try {
    state.value = await hydrateContextControlsState()
  } catch (nextError) {
    error.value = nextError instanceof Error ? nextError.message : t('shell.context.error')
    state.value = {
      workspaces: [],
      projects: [],
      selectedWorkspaceId: '',
      selectedProjectId: '',
    }
  } finally {
    loading.value = false
  }
}

async function handleWorkspaceChange(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  loading.value = true
  error.value = null

  try {
    state.value = await switchContextWorkspace(value)
  } catch (nextError) {
    error.value = nextError instanceof Error ? nextError.message : t('shell.context.error')
  } finally {
    loading.value = false
  }
}

function handleProjectChange(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  state.value = {
    ...state.value,
    selectedProjectId: value,
  }
  setSelectedProjectId(value)
}

watch(
  () => route.fullPath,
  () => {
    if (loading.value) {
      return
    }

    const workspaceId = getSelectedWorkspaceId()
    const projectId = getSelectedProjectId()

    if (workspaceId && workspaceId !== state.value.selectedWorkspaceId) {
      void hydrateContext()
      return
    }

    if (projectId && projectId !== state.value.selectedProjectId) {
      state.value = {
        ...state.value,
        selectedProjectId: projectId,
      }
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
        <p class="app-context-controls__summary">{{ summaryCopy }}</p>
      </div>

      <RouterLink v-if="isAdvancedRoute" to="/documents" class="app-context-controls__back-link">
        {{ t('shell.context.backToDocuments') }}
      </RouterLink>
    </div>

    <p v-if="error" class="app-context-controls__error">
      {{ error }}
    </p>

    <p
      v-else-if="!presentation.hasWorkspaces || presentation.status === 'workspace_only'"
      class="app-context-controls__empty"
    >
      {{
        presentation.hasWorkspaces
          ? t('shell.context.emptyLibraryHint')
          : t('shell.context.emptyWorkspaceHint')
      }}
    </p>

    <div class="app-context-controls__fields">
      <label class="app-context-controls__field">
        <span>{{ t('shell.context.workspace') }}</span>
        <select
          class="app-context-controls__select"
          :value="state.selectedWorkspaceId"
          :disabled="loading || !state.workspaces.length"
          @change="void handleWorkspaceChange($event)"
        >
          <option value="">{{ t('shell.context.none') }}</option>
          <option v-for="workspace in state.workspaces" :key="workspace.id" :value="workspace.id">
            {{ workspace.name }}
          </option>
        </select>
        <small v-if="!presentation.hasWorkspaceChoices && selectedWorkspace">{{
          t('shell.context.defaultWorkspace')
        }}</small>
      </label>

      <label class="app-context-controls__field">
        <span>{{ t('shell.context.library') }}</span>
        <select
          class="app-context-controls__select"
          :value="state.selectedProjectId"
          :disabled="loading || !state.projects.length"
          @change="handleProjectChange"
        >
          <option value="">{{ t('shell.context.none') }}</option>
          <option v-for="project in state.projects" :key="project.id" :value="project.id">
            {{ project.name }}
          </option>
        </select>
        <small v-if="!presentation.hasProjectChoices && selectedProject">{{
          t('shell.context.defaultLibrary')
        }}</small>
      </label>
    </div>

    <details
      v-if="presentation.showAdvancedActions || isAdvancedRoute"
      class="app-context-controls__advanced"
    >
      <summary>{{ t('shell.context.advanced') }}</summary>
      <p>{{ t('shell.context.advancedHint') }}</p>
      <ul class="app-context-controls__advanced-list">
        <li>{{ t('shell.context.advancedCreate') }}</li>
        <li>{{ t('shell.context.advancedManage') }}</li>
      </ul>
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

.app-context-controls__error,
.app-context-controls__empty {
  margin: 0;
  font-size: 0.84rem;
}

.app-context-controls__error {
  color: var(--rr-color-danger-700, #b42318);
}

.app-context-controls__empty {
  color: var(--rr-color-text-secondary);
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
.app-context-controls__advanced p,
.app-context-controls__advanced-list {
  color: var(--rr-color-text-secondary);
}

.app-context-controls__field span {
  font-size: 0.78rem;
  font-weight: 700;
}

.app-context-controls__field small,
.app-context-controls__advanced p,
.app-context-controls__advanced-list {
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

.app-context-controls__advanced-list {
  margin: 0 0 10px;
  padding-left: 18px;
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
