<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'

import {
  createSource,
  fetchDocuments,
  fetchProjects,
  fetchSources,
  fetchWorkspaces,
  ingestText,
  type DocumentSummary,
  type SourceSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  setSelectedWorkspaceId,
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
  workspace_id: string
}

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const documents = ref<DocumentSummary[]>([])
const sources = ref<SourceSummary[]>([])
const sourceLabel = ref('Pasted text')
const externalKey = ref(`note-${String(Date.now())}`)
const title = ref('')
const text = ref('')
const statusMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(false)

const selectedProjectId = computed(() => getSelectedProjectId())
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const pageStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: 'Select a project in Setup' }
  }

  if (documents.value.length > 0) {
    return { status: 'ready', label: `${String(documents.value.length)} indexed documents` }
  }

  return { status: 'draft', label: 'Ready for first ingest' }
})

async function loadProjectData(projectId: string) {
  const [docs, srcs] = await Promise.all([fetchDocuments(projectId), fetchSources(projectId)])
  documents.value = docs
  sources.value = srcs
}

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
  if (selectedProjectId.value) {
    await loadProjectData(selectedProjectId.value)
  }
})

async function ingestCurrentText() {
  errorMessage.value = null
  statusMessage.value = null
  loading.value = true

  if (!selectedProjectId.value) {
    errorMessage.value = 'Create and select a project first in Setup.'
    loading.value = false
    return
  }

  try {
    let sourceId = sources.value[0]?.id
    if (!sourceId) {
      const source = await createSource({
        project_id: selectedProjectId.value,
        source_kind: 'text',
        label: sourceLabel.value.trim() || 'Pasted text',
      })
      sourceId = source.id
      sources.value = [source, ...sources.value.filter((item) => item.id !== source.id)]
    }

    const result = await ingestText({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      external_key: externalKey.value.trim(),
      title: title.value.trim() || null,
      text: text.value,
    })

    await loadProjectData(selectedProjectId.value)

    statusMessage.value = `Indexed ${String(result.chunk_count)} chunks into document ${result.document_id}.`
    text.value = ''
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Failed to ingest text'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <section class="rr-page-grid ingestion-page">
    <PageSection
      eyebrow="Step 2"
      title="Ingest text into the active project"
      description="Create a simple text source if needed, send content through indexing, and keep the resulting documents visible alongside the current project context."
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <template #actions>
        <RouterLink class="rr-button rr-button--secondary" to="/ask">
          Continue to ask
        </RouterLink>
      </template>

      <div class="rr-stat-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">Workspace</p>
          <strong>{{ selectedWorkspace?.name ?? 'Not selected' }}</strong>
          <p>The active workspace stays visible while you prepare content.</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Project</p>
          <strong>{{ selectedProject?.name ?? 'Not selected' }}</strong>
          <p>{{ selectedProject ? 'Ingestion targets this project only.' : 'Setup must establish project context first.' }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Documents</p>
          <strong>{{ documents.length }}</strong>
          <p>Indexed documents currently available for grounded retrieval.</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">Sources</p>
          <strong>{{ sources.length }}</strong>
          <p>Simple mode starts with a single text source and grows from there.</p>
        </article>
      </div>

      <p
        v-if="statusMessage"
        class="rr-banner"
        data-tone="success"
      >
        {{ statusMessage }}
      </p>
      <p
        v-if="errorMessage"
        class="rr-banner"
        data-tone="danger"
      >
        {{ errorMessage }}
      </p>

      <div class="ingestion-grid">
        <article class="rr-panel rr-panel--accent rr-stack">
          <div class="ingestion-panel__heading">
            <div>
              <p class="rr-kicker">Paste text</p>
              <h3>Send content to the indexing pipeline</h3>
            </div>
            <StatusBadge
              :status="selectedProjectId ? 'ready' : 'blocked'"
              :label="selectedProjectId ? 'Project selected' : 'Needs setup'"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">Source label</span>
              <input
                v-model="sourceLabel"
                class="rr-control"
                type="text"
                placeholder="Pasted text"
              >
            </label>
            <div class="rr-form-grid rr-form-grid--two">
              <label class="rr-field">
                <span class="rr-field__label">External key</span>
                <input
                  v-model="externalKey"
                  class="rr-control"
                  type="text"
                  placeholder="note-001"
                >
              </label>
              <label class="rr-field">
                <span class="rr-field__label">Title</span>
                <input
                  v-model="title"
                  class="rr-control"
                  type="text"
                  placeholder="Internal handbook excerpt"
                >
              </label>
            </div>
            <label class="rr-field">
              <span class="rr-field__label">Text content</span>
              <textarea
                v-model="text"
                class="rr-control"
                rows="12"
                placeholder="Paste the content you want RustRAG to index"
              />
              <p class="rr-field__hint">
                Keep this minimal flow simple: one selected project, one source label, one text paste.
              </p>
            </label>
          </div>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!selectedProjectId || !text.trim() || loading"
              @click="ingestCurrentText"
            >
              {{ loading ? 'Indexing…' : 'Ingest text' }}
            </button>
          </div>
        </article>

        <div class="ingestion-side rr-grid">
          <article class="rr-panel">
            <div class="ingestion-panel__heading">
              <div>
                <p class="rr-kicker">Indexed content</p>
                <h3>Documents available to Ask</h3>
              </div>
              <StatusBadge :status="documents.length ? 'ready' : 'draft'" :label="documents.length ? 'Indexed' : 'Empty'" />
            </div>

            <p v-if="!documents.length" class="rr-note">
              No indexed documents yet. Paste content on the left to create the first one.
            </p>
            <ul v-else class="rr-list">
              <li v-for="document in documents" :key="document.id">
                <strong>{{ document.title || document.external_key }}</strong>
                <span class="rr-muted">{{ document.status ?? 'Indexed' }}</span>
              </li>
            </ul>
          </article>

          <article class="rr-panel rr-panel--muted">
            <div class="ingestion-panel__heading">
              <div>
                <p class="rr-kicker">Source registry</p>
                <h3>Known sources for the project</h3>
              </div>
              <StatusBadge :status="sources.length ? 'ready' : 'draft'" :label="sources.length ? 'Present' : 'Will be created'" />
            </div>

            <p v-if="!sources.length" class="rr-note">
              The first ingest automatically creates a text source if one does not exist yet.
            </p>
            <ul v-else class="rr-list">
              <li v-for="source in sources" :key="source.id">
                <strong>{{ source.label }}</strong>
                <span class="rr-muted">{{ source.source_kind }} · {{ source.status }}</span>
              </li>
            </ul>
          </article>
        </div>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.ingestion-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.3fr) minmax(320px, 0.7fr);
  gap: var(--rr-space-4);
}

.ingestion-panel__heading {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.ingestion-panel__heading h3 {
  margin: 4px 0 0;
}

@media (width <= 1100px) {
  .ingestion-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 700px) {
  .ingestion-panel__heading {
    flex-direction: column;
  }
}
</style>
