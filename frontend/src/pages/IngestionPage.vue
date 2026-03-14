<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

import { useDocumentsStore } from 'src/stores/documents'
import { useFlowStore } from 'src/stores/flow'
import { useProjectsStore } from 'src/stores/projects'
import { useWorkspacesStore } from 'src/stores/workspaces'

const flowStore = useFlowStore()
const documentsStore = useDocumentsStore()
const workspacesStore = useWorkspacesStore()
const projectsStore = useProjectsStore()

const sourceLabel = ref('Pasted text')
const externalKey = ref(`note-${Date.now()}`)
const title = ref('')
const text = ref('')
const statusMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)

const selectedProjectId = computed(() => flowStore.projectId)
const selectedProject = computed(
  () => projectsStore.items.find((item) => item.id === flowStore.projectId) ?? null,
)
const selectedWorkspace = computed(
  () => workspacesStore.items.find((item) => item.id === flowStore.workspaceId) ?? null,
)
const projectState = computed(() =>
  selectedProjectId.value ? documentsStore.byProjectId[selectedProjectId.value] ?? null : null,
)

onMounted(async () => {
  const workspaces = await workspacesStore.fetchList()
  if (!flowStore.workspaceId && workspaces.length > 0) {
    flowStore.selectWorkspace(workspaces[0]?.id ?? '')
  }
  if (flowStore.workspaceId) {
    const projects = await projectsStore.fetchList(flowStore.workspaceId)
    if (!flowStore.projectId && projects.length > 0) {
      flowStore.selectProject(projects[0]?.id ?? '')
    }
  }
  if (selectedProjectId.value) {
    await Promise.all([
      documentsStore.fetchProjectDocuments(selectedProjectId.value),
      documentsStore.fetchProjectJobs(selectedProjectId.value),
      documentsStore.fetchProjectSources(selectedProjectId.value),
    ])
  }
})

async function ingestCurrentText() {
  errorMessage.value = null
  statusMessage.value = null

  if (!selectedProjectId.value) {
    errorMessage.value = 'Create and select a project first in Setup.'
    return
  }

  try {
    let sourceId = projectState.value?.sources.data[0]?.id
    if (!sourceId) {
      const source = await documentsStore.createSourceForProject({
        project_id: selectedProjectId.value,
        source_kind: 'text',
        label: sourceLabel.value.trim() || 'Pasted text',
      })
      sourceId = source.id
    }

    const result = await documentsStore.ingestTextForProject({
      project_id: selectedProjectId.value,
      source_id: sourceId,
      external_key: externalKey.value.trim(),
      title: title.value.trim() || null,
      text: text.value,
    })

    await Promise.all([
      documentsStore.fetchProjectDocuments(selectedProjectId.value),
      documentsStore.fetchProjectJobs(selectedProjectId.value),
      documentsStore.fetchProjectSources(selectedProjectId.value),
    ])

    statusMessage.value = `Indexed ${result.chunkCount} chunks into document ${result.documentId}.`
    text.value = ''
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Failed to ingest text'
  }
}
</script>

<template>
  <section class="ingestion-page">
    <header>
      <h2>Ingest</h2>
      <p>Paste text into the selected project and send it to the indexing pipeline.</p>
    </header>

    <div class="context-card">
      <p><strong>Workspace:</strong> {{ selectedWorkspace?.name ?? 'not selected' }}</p>
      <p><strong>Project:</strong> {{ selectedProject?.name ?? 'not selected' }}</p>
      <p v-if="!selectedProject">Go to Setup first and create/select a project.</p>
    </div>

    <p v-if="statusMessage" class="success-banner">{{ statusMessage }}</p>
    <p v-if="errorMessage" class="error-banner">{{ errorMessage }}</p>

    <div class="ingestion-grid">
      <article class="panel">
        <h3>Paste text</h3>
        <label class="field">
          <span>Source label</span>
          <input v-model="sourceLabel" type="text" placeholder="Pasted text">
        </label>
        <label class="field">
          <span>External key</span>
          <input v-model="externalKey" type="text" placeholder="note-001">
        </label>
        <label class="field">
          <span>Title</span>
          <input v-model="title" type="text" placeholder="Internal handbook excerpt">
        </label>
        <label class="field">
          <span>Text content</span>
          <textarea v-model="text" rows="12" placeholder="Paste the content you want RustRAG to index" />
        </label>

        <button type="button" :disabled="!selectedProjectId || !text.trim() || documentsStore.ingestState.status === 'loading'" @click="ingestCurrentText">
          {{ documentsStore.ingestState.status === 'loading' ? 'Indexing…' : 'Ingest text' }}
        </button>
      </article>

      <article class="panel">
        <h3>Indexed content</h3>
        <p v-if="!projectState?.documents.data.length">No indexed documents yet.</p>
        <ul v-else>
          <li v-for="document in projectState.documents.data" :key="document.id">
            {{ document.title || document.external_key }}
          </li>
        </ul>

        <h3>Recent jobs</h3>
        <p v-if="!projectState?.jobs.data.length">No ingestion jobs yet.</p>
        <ul v-else>
          <li v-for="job in projectState.jobs.data" :key="job.id">
            {{ job.status }} / {{ job.stage }}
          </li>
        </ul>
      </article>
    </div>
  </section>
</template>

<style scoped>
.ingestion-page {
  display: grid;
  gap: 16px;
}

.context-card,
.panel {
  padding: 16px;
  border: 1px solid #d7dee7;
  border-radius: 16px;
  background: #f8fbff;
}

.ingestion-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.35fr) minmax(320px, 0.65fr);
  gap: 16px;
}

.panel {
  display: grid;
  gap: 12px;
}

.field {
  display: grid;
  gap: 6px;
}

input,
textarea {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #c8d5e3;
  border-radius: 10px;
  font: inherit;
  background: #fff;
}

button {
  width: fit-content;
  padding: 10px 16px;
  border: 0;
  border-radius: 999px;
  background: #215dff;
  color: #fff;
  font: inherit;
  font-weight: 600;
  cursor: pointer;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.error-banner,
.success-banner {
  padding: 12px 14px;
  border-radius: 10px;
}

.error-banner {
  background: #fde2e2;
  color: #b42318;
}

.success-banner {
  background: #dcfce7;
  color: #166534;
}

@media (width <= 1100px) {
  .ingestion-grid {
    grid-template-columns: 1fr;
  }
}
</style>
