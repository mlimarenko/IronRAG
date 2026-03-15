<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import AppPanel from 'src/components/ui/AppPanel.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
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

const { t } = useI18n()

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
    return t('flow.overview.stats.next.setup')
  }

  if (!hasProject.value) {
    return t('flow.overview.stats.next.project')
  }

  return t('flow.overview.stats.next.ingest')
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const activeWorkspaceId = syncSelectedWorkspaceId(workspaces.value)
  if (activeWorkspaceId) {
    projects.value = await fetchProjects(activeWorkspaceId)
    syncSelectedProjectId(projects.value)
  } else {
    projects.value = []
    syncSelectedProjectId([])
  }
})
</script>

<template>
  <section class="rr-page-grid overview-page">
    <PageSection
      :title="t('flow.overview.title')"
      :description="t('flow.overview.description')"
      status="focused"
      :status-label="t('shell.status.focused')"
    >
      <template #actions>
        <RouterLink class="rr-button" to="/processing">
          {{ t('flow.overview.cta') }}
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.workspace.label') }}</p>
          <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.project.label') }}</p>
          <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.next.label') }}</p>
          <strong>{{ nextAction }}</strong>
        </article>
      </div>

      <AppPanel class="flow-shortcuts" tone="muted" eyebrow="Path" :title="nextAction">
        <p class="flow-shortcuts__title">{{ nextAction }}</p>
        <div class="flow-shortcuts__actions">
          <RouterLink class="rr-button rr-button--secondary" to="/processing">
            {{ t('flow.overview.cards.workspace.title') }}
          </RouterLink>
          <RouterLink class="rr-button rr-button--secondary" to="/files">
            {{ t('flow.overview.cards.library.title') }}
          </RouterLink>
          <RouterLink class="rr-button rr-button--secondary" to="/search">
            {{ t('flow.overview.cards.search.title') }}
          </RouterLink>
        </div>
      </AppPanel>
    </PageSection>
  </section>
</template>

<style scoped>
.flow-shortcuts {
  gap: var(--rr-space-4);
}

.flow-shortcuts__title {
  margin: 0;
  font-size: 0.98rem;
  font-weight: 650;
  color: var(--rr-color-text-secondary);
}

.flow-shortcuts__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
}
</style>
