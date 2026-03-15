<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

import {
  fetchProjects,
  fetchProjectReadiness,
  fetchWorkspaces,
  isUnauthorizedApiError,
  type ProjectReadinessSummary,
  type ProjectSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import AppPanel from 'src/components/ui/AppPanel.vue'
import StatusBanner from 'src/components/ui/StatusBanner.vue'
import {
  ensureProjectMatchesWorkspace,
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  syncWorkspaceProjectScope,
} from 'src/stores/flow'

const { t } = useI18n()

const workspaces = ref<WorkspaceSummary[]>([])
const projects = ref<ProjectSummary[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const errorMessage = ref<string | null>(null)
const infoMessage = ref<string | null>(null)
const selectedProjectId = ref<string>(getSelectedProjectId())
const loading = ref(true)

function extractErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : t('common.messages.unknownError')
}

async function loadReadiness(id: string) {
  const scopedId = ensureProjectMatchesWorkspace(projects.value, id)
  if (!scopedId) {
    readiness.value = null
    selectedProjectId.value = ''
    return
  }

  errorMessage.value = null
  infoMessage.value = null
  selectedProjectId.value = scopedId
  setSelectedProjectId(scopedId)

  try {
    readiness.value = await fetchProjectReadiness(scopedId)
  } catch (error) {
    const message = extractErrorMessage(error)
    if (isUnauthorizedApiError(error)) {
      infoMessage.value =
        'Project list is visible, but readiness details require an authorized API token.'
      readiness.value = null
    } else {
      errorMessage.value = message
    }
  }
}

const selectedProject = computed(
  () => projects.value.find((project) => project.id === selectedProjectId.value) ?? null,
)

const pageStatus = computed(() => {
  if (errorMessage.value) {
    return { status: 'blocked', label: 'Readiness unavailable' }
  }

  if (loading.value) {
    return { status: 'pending', label: 'Loading project surfaces' }
  }

  if (projects.value.length === 0) {
    return { status: 'draft', label: 'No projects yet' }
  }

  if (readiness.value?.ready_for_query) {
    return { status: 'ready', label: 'Query-ready project' }
  }

  return { status: 'partial', label: 'Inventory loaded' }
})

watch(selectedProjectId, async (value, previousValue) => {
  if (!value || value === previousValue) {
    return
  }

  await loadReadiness(value)
})

onMounted(async () => {
  try {
    workspaces.value = await fetchWorkspaces()
    const workspaceId =
      getSelectedWorkspaceId() || syncWorkspaceProjectScope(workspaces.value, []).workspaceId

    if (!workspaceId) {
      projects.value = []
      selectedProjectId.value = ''
      infoMessage.value =
        'No workspace selected yet. Choose a workspace from the navigation before inspecting libraries.'
      return
    }

    projects.value = await fetchProjects(workspaceId)
    const scope = syncWorkspaceProjectScope(workspaces.value, projects.value)
    selectedProjectId.value = scope.projectId

    if (projects.value.length === 0) {
      infoMessage.value =
        'No libraries created yet in this workspace. Create one from Advanced context to start adding documents.'
      return
    }

    if (scope.projectId) {
      await loadReadiness(scope.projectId)
    }
  } catch (error) {
    errorMessage.value = extractErrorMessage(error)
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <section class="rr-page-grid">
    <PageSection
      eyebrow="Operations"
      title="Projects"
      description="Projects are the primary RAG work surface inside a workspace. This page now sits on shared section, panel, banner, and empty-state patterns instead of one-off local CSS."
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <ErrorStateCard
        v-if="errorMessage"
        title="Project surfaces unavailable"
        :message="errorMessage"
        detail="Workspace discovery and readiness checks should fail as structured state, not bare text."
      />

      <div v-else-if="loading" class="rr-grid rr-grid--two">
        <LoadingSkeletonPanel title="Loading projects" />
        <LoadingSkeletonPanel title="Loading readiness" />
      </div>

      <div v-else class="rr-grid rr-grid--two">
        <AppPanel
          eyebrow="Inventory"
          title="Projects"
          description="Inspect available project scopes before touching ingestion or query."
          tone="accent"
          :status="projects.length > 0 ? 'ready' : 'draft'"
          :status-label="projects.length > 0 ? `${projects.length} loaded` : 'No inventory'"
        >
          <EmptyStateCard
            v-if="projects.length === 0"
            title="No projects found"
            :message="
              infoMessage ??
              'Create a workspace and at least one project before querying readiness.'
            "
            hint="Once projects exist, use the same shared panel layout to inspect indexing posture."
          />

          <ul v-else class="rr-list project-list">
            <li v-for="project in projects" :key="project.id">
              <div class="project-list__row">
                <div class="project-list__copy">
                  <strong>{{ project.name }}</strong>
                  <p>{{ project.slug }}</p>
                </div>

                <button
                  type="button"
                  class="rr-button rr-button--secondary"
                  @click="loadReadiness(project.id)"
                >
                  Inspect readiness
                </button>
              </div>
            </li>
          </ul>
        </AppPanel>

        <AppPanel
          eyebrow="Health"
          title="Readiness"
          description="Project readiness stays explicit about indexing state, document inventory, and whether query is safe to enable."
          :status="readiness?.ready_for_query ? 'ready' : selectedProjectId ? 'partial' : 'draft'"
          :status-label="selectedProject?.name ?? 'No project selected'"
        >
          <StatusBanner
            v-if="infoMessage && projects.length > 0 && !readiness"
            tone="info"
            :message="infoMessage"
          />

          <EmptyStateCard
            v-else-if="!selectedProjectId"
            title="Select a project"
            message="Readiness details will appear here after you choose a project from the inventory panel."
          />

          <template v-else-if="readiness">
            <div class="rr-stat-strip">
              <article class="rr-stat">
                <p class="rr-stat__label">Indexing state</p>
                <strong>{{ readiness.indexing_state }}</strong>
              </article>
              <article class="rr-stat">
                <p class="rr-stat__label">Ready for query</p>
                <strong>{{ readiness.ready_for_query ? 'Yes' : 'No' }}</strong>
              </article>
              <article class="rr-stat">
                <p class="rr-stat__label">Sources</p>
                <strong>{{ readiness.sources }}</strong>
              </article>
              <article class="rr-stat">
                <p class="rr-stat__label">Documents</p>
                <strong>{{ readiness.documents }}</strong>
              </article>
              <article class="rr-stat">
                <p class="rr-stat__label">Ingestion jobs</p>
                <strong>{{ readiness.ingestion_jobs }}</strong>
              </article>
            </div>
          </template>

          <EmptyStateCard
            v-else
            title="No readiness data loaded"
            :message="infoMessage ?? 'Select a project to inspect current indexing posture.'"
          />
        </AppPanel>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.project-list__row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--rr-space-4);
}

.project-list__copy {
  display: grid;
  gap: 4px;
}

.project-list__copy strong,
.project-list__copy p {
  margin: 0;
}

.project-list__copy p {
  color: var(--rr-color-text-muted);
}

@media (width <= 700px) {
  .project-list__row {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
