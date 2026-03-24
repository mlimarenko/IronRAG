<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import router from 'src/router'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import AppendDocumentDialog from 'src/components/documents/AppendDocumentDialog.vue'
import DocumentInspectorPane from 'src/components/documents/DocumentInspectorPane.vue'
import DocumentsEmptyState from 'src/components/documents/DocumentsEmptyState.vue'
import DocumentsFiltersBar from 'src/components/documents/DocumentsFiltersBar.vue'
import DocumentsList from 'src/components/documents/DocumentsList.vue'
import DocumentsWorkspaceHeader from 'src/components/documents/DocumentsWorkspaceHeader.vue'
import ReplaceDocumentDialog from 'src/components/documents/ReplaceDocumentDialog.vue'
import { downloadDocumentExtractedText } from 'src/services/api/documents'
import { useDocumentsStore } from 'src/stores/documents'
import { useShellStore } from 'src/stores/shell'

const documentsStore = useDocumentsStore()
const shellStore = useShellStore()
const headerRef = ref<InstanceType<typeof DocumentsWorkspaceHeader> | null>(null)
const {
  mergedRows,
  filteredRows,
  refreshIntervalMs,
  workspace,
  mutationLoading,
  mutationError,
  appendDialogDocumentId,
  replaceDialogDocumentId,
} = storeToRefs(documentsStore)

let refreshTimer: number | null = null
function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

watch(
  () => shellStore.context?.activeLibrary.id ?? null,
  async (libraryId) => {
    if (!libraryId) {
      return
    }
    documentsStore.clearUploadFailures()
    documentsStore.closeDetail()
    await documentsStore.loadWorkspace()
  },
  { immediate: true },
)

watch(
  () => refreshIntervalMs.value,
  (intervalMs) => {
    stopPolling()
    if (intervalMs <= 0) {
      return
    }
    refreshTimer = window.setInterval(() => {
      void documentsStore.loadWorkspace({ syncInspector: true }).catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
})

const hasActiveFilters = computed(
  () =>
    Boolean(workspace.value.filters.searchQuery.trim()) ||
    workspace.value.filters.statusFilter !== '',
)

const hasDocuments = computed(
  () => Boolean(workspace.value.rows.length || workspace.value.uploadQueue.length),
)
const detail = computed(() => workspace.value.inspector.detail)
const detailLoading = computed(() => workspace.value.inspector.loading)
const detailError = computed(() => workspace.value.inspector.error)
const detailOpen = computed(() => Boolean(workspace.value.selectedDocumentId))
const showInspector = computed(
  () => detailOpen.value && Boolean(detail.value || detailLoading.value || detailError.value),
)
const activeBacklogCount = computed(
  () => workspace.value.counters.queued + workspace.value.counters.processing,
)

async function openInGraph(graphNodeId: string) {
  await router.push({
    path: '/graph',
    query: { node: graphNodeId },
  })
}

async function downloadText(id: string) {
  const detailRow = detail.value
  const blob = await downloadDocumentExtractedText(id)
  const url = window.URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = detailRow?.fileName
    ? `${detailRow.fileName.replace(/\.[^.]+$/, '')}-extracted.txt`
    : 'document-extracted.txt'
  document.body.append(anchor)
  anchor.click()
  anchor.remove()
  window.URL.revokeObjectURL(url)
}

async function submitAppend(content: string) {
  if (!appendDialogDocumentId.value) {
    return
  }
  await documentsStore.submitAppendDocument(appendDialogDocumentId.value, content)
}

async function submitReplace(file: File) {
  if (!replaceDialogDocumentId.value) {
    return
  }
  await documentsStore.submitReplaceDocument(replaceDialogDocumentId.value, file)
}

function triggerUpload(): void {
  headerRef.value?.openUploader?.()
}

function clearFilters(): void {
  documentsStore.setSearchQuery('')
  documentsStore.setStatusFilter('')
}
</script>

<template>
  <PageSurface mode="full">
    <div class="rr-documents-page">
      <DocumentsWorkspaceHeader
        ref="headerRef"
        :accepted-formats="workspace.acceptedFormats"
        :max-size-mb="workspace.maxSizeMb"
        :loading="workspace.uploadInProgress"
        :total-count="workspace.rows.length"
        :active-count="activeBacklogCount"
        :upload-failures="workspace.uploadFailures"
        :has-documents="hasDocuments"
        @select="documentsStore.uploadFiles"
        @clear-failures="documentsStore.clearUploadFailures"
      />

      <section
        v-if="!workspace.error || hasDocuments"
        class="rr-documents-page__workspace rr-documents__workspace-shell"
        :class="{ 'has-inspector': showInspector }"
      >
        <div class="rr-documents-page__primary">
          <section class="rr-documents-page__panel">
            <DocumentsFiltersBar
              v-if="hasDocuments || hasActiveFilters"
              :search-query="workspace.filters.searchQuery"
              :status-filter="workspace.filters.statusFilter"
              :visible-count="filteredRows.length"
              :total-count="mergedRows.length"
              :show-meta="hasActiveFilters && filteredRows.length !== mergedRows.length"
              @update-search="documentsStore.setSearchQuery"
              @update-status="documentsStore.setStatusFilter"
            />

            <DocumentsList
              v-if="filteredRows.length"
              :rows="filteredRows"
              :selected-id="detailOpen ? detail?.id ?? null : null"
              @detail="documentsStore.openDetail"
              @append="documentsStore.openAppendDialog"
              @replace="documentsStore.openReplaceDialog"
              @retry="documentsStore.retryDocument"
              @remove="documentsStore.removeDocument"
            />

            <DocumentsEmptyState
              v-else
              :loading="workspace.loading"
              :has-documents="hasDocuments"
              :has-active-filters="hasActiveFilters"
              @upload="triggerUpload"
              @clear-filters="clearFilters"
            />
          </section>
        </div>

        <div
          v-if="showInspector"
          class="rr-documents-page__inspector-column"
          :class="{ 'is-open': showInspector }"
        >
          <DocumentInspectorPane
            :open="detailOpen"
            :detail="detail"
            :loading="detailLoading"
            :error="detailError"
            @close="documentsStore.closeDetail"
            @append="documentsStore.openAppendDialog"
            @replace="documentsStore.openReplaceDialog"
            @retry="documentsStore.retryDocument"
            @remove="documentsStore.removeDocument"
            @open-in-graph="openInGraph"
            @download-text="downloadText"
          />
        </div>

        <button
          v-if="showInspector"
          type="button"
          class="rr-documents-page__inspector-backdrop"
          :aria-label="$t('dialogs.close')"
          @click="documentsStore.closeDetail"
        />
      </section>

      <ErrorStateCard
        v-else
        :title="$t('documents.workspace.title')"
        :description="workspace.error ?? $t('documents.loading')"
      />
    </div>

    <AppendDocumentDialog
      :open="Boolean(appendDialogDocumentId)"
      :document-name="detail?.fileName ?? null"
      :loading="mutationLoading"
      :error="mutationError"
      @close="documentsStore.closeAppendDialog"
      @submit="submitAppend"
    />

    <ReplaceDocumentDialog
      :open="Boolean(replaceDialogDocumentId)"
      :document-name="detail?.fileName ?? null"
      :accepted-formats="workspace.acceptedFormats"
      :loading="mutationLoading"
      :error="mutationError"
      @close="documentsStore.closeReplaceDialog"
      @submit="submitReplace"
    />
  </PageSurface>
</template>

<style scoped lang="scss">
.rr-documents-page {
  display: grid;
  gap: 1.1rem;
  width: min(100%, 1760px);
  margin: 0 auto;
}

.rr-documents-page__workspace {
  position: relative;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 1rem;
  align-items: start;
}

.rr-documents-page__workspace.has-inspector {
  grid-template-columns: minmax(30rem, 0.8fr) minmax(0, 1.2fr);
}

.rr-documents-page__primary,
.rr-documents-page__inspector-column {
  min-width: 0;
}

.rr-documents-page__panel {
  display: grid;
  gap: 0.9rem;
  min-height: calc(100vh - 9.8rem);
  padding: 1rem;
  border: 1px solid var(--rr-border-soft);
  border-radius: var(--rr-radius-lg);
  background: var(--rr-bg-panel);
  box-shadow: none;
}

.rr-documents-page__inspector-column {
  position: relative;
  display: grid;
  align-self: start;
}

.rr-documents-page__inspector-backdrop {
  display: none;
  border: 0;
  padding: 0;
}

.rr-documents-page__inspector-column :deep(.rr-document-inspector) {
  position: sticky;
  top: 5.7rem;
  min-height: calc(100vh - 8.7rem);
  max-height: calc(100vh - 8.7rem);
  overflow: auto;
}

@media (max-width: 1080px) {
  .rr-documents-page__workspace.has-inspector {
    grid-template-columns: 1fr;
  }

  .rr-documents-page__panel,
  .rr-documents-page__inspector-column :deep(.rr-document-inspector) {
    min-height: 28rem;
    max-height: none;
  }

  .rr-documents-page__inspector-backdrop {
    position: fixed;
    inset: 0;
    z-index: 19;
    display: block;
    background: rgba(15, 23, 42, 0.12);
    backdrop-filter: blur(4px);
  }

  .rr-documents-page__inspector-column {
    position: fixed;
    inset: auto 1rem 1rem 1rem;
    top: 5.2rem;
    z-index: 20;
    opacity: 0;
    pointer-events: none;
    transform: translateY(1rem);
    transition:
      opacity 0.18s ease,
      transform 0.18s ease;
  }

  .rr-documents-page__inspector-column.is-open {
    opacity: 1;
    pointer-events: auto;
    transform: translateY(0);
  }

  .rr-documents-page__inspector-column:not(.is-open) {
    display: none;
  }
}

@media (max-width: 720px) {
  .rr-documents-page {
    gap: 1rem;
  }

  .rr-documents-page__panel {
    padding: 0.8rem;
    border-radius: 1rem;
  }

  .rr-documents-page__inspector-column.is-open {
    position: fixed;
    inset: auto 0 0 0;
    z-index: 20;
    padding: 0.75rem;
    top: auto;
    right: 0;
    bottom: 0;
    width: auto;
    transform: translateY(0);
  }
}
</style>
