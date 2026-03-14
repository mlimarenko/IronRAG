<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
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
      :eyebrow="t('flow.overview.eyebrow')"
      :title="t('flow.overview.title')"
      :description="t('flow.overview.description')"
      status="focused"
      :status-label="t('shell.status.focused')"
    >
      <template #actions>
        <RouterLink class="rr-button" to="/setup">
          {{ t('flow.overview.cta') }}
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.workspace.label') }}</p>
          <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
          <p>
            {{
              hasWorkspace
                ? t('flow.overview.stats.workspace.ready')
                : t('flow.overview.stats.workspace.empty')
            }}
          </p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.project.label') }}</p>
          <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
          <p>
            {{
              hasProject
                ? t('flow.overview.stats.project.ready')
                : t('flow.overview.stats.project.empty')
            }}
          </p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.overview.stats.next.label') }}</p>
          <strong>{{ nextAction }}</strong>
        </article>
      </div>

      <div class="rr-grid rr-grid--cards">
        <article class="flow-card rr-panel" data-state="workspace">
          <div class="flow-card__header">
            <h3>{{ t('flow.overview.cards.workspace.title') }}</h3>
            <StatusBadge
              :status="hasProject ? 'ready' : hasWorkspace ? 'partial' : 'draft'"
              :label="
                hasProject
                  ? t('flow.overview.cards.workspace.ready')
                  : hasWorkspace
                    ? t('flow.overview.cards.workspace.partial')
                    : t('flow.overview.cards.workspace.draft')
              "
            />
          </div>
          <p>{{ t('flow.overview.cards.workspace.body') }}</p>
          <RouterLink class="rr-button rr-button--secondary" to="/setup">
            {{ t('flow.overview.cards.workspace.action') }}
          </RouterLink>
        </article>

        <article class="flow-card rr-panel" data-state="library">
          <div class="flow-card__header">
            <h3>{{ t('flow.overview.cards.library.title') }}</h3>
            <StatusBadge
              :status="selectedProject ? 'ready' : 'blocked'"
              :label="
                selectedProject
                  ? t('flow.overview.cards.library.ready')
                  : t('flow.overview.cards.library.blocked')
              "
            />
          </div>
          <p>{{ t('flow.overview.cards.library.body') }}</p>
          <RouterLink class="rr-button rr-button--secondary" to="/ingest">
            {{ t('flow.overview.cards.library.action') }}
          </RouterLink>
        </article>

        <article class="flow-card rr-panel" data-state="search">
          <div class="flow-card__header">
            <h3>{{ t('flow.overview.cards.search.title') }}</h3>
            <StatusBadge
              :status="selectedProject ? 'ready' : 'blocked'"
              :label="
                selectedProject
                  ? t('flow.overview.cards.search.ready')
                  : t('flow.overview.cards.search.blocked')
              "
            />
          </div>
          <p>{{ t('flow.overview.cards.search.body') }}</p>
          <RouterLink class="rr-button rr-button--secondary" to="/ask">
            {{ t('flow.overview.cards.search.action') }}
          </RouterLink>
        </article>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.flow-card {
  gap: var(--rr-space-4);
}

.flow-card__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
}

.flow-card h3,
.flow-card p {
  margin: 0;
}

@media (width <= 700px) {
  .flow-card__header {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
