<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import router from 'src/router'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import AppendDocumentDialog from 'src/components/documents/AppendDocumentDialog.vue'
import DocumentInspectorPane from 'src/components/documents/DocumentInspectorPane.vue'
import DocumentsEmptyState from 'src/components/documents/DocumentsEmptyState.vue'
import DocumentsFiltersBar from 'src/components/documents/DocumentsFiltersBar.vue'
import DocumentsList from 'src/components/documents/DocumentsList.vue'
import DocumentsWorkspaceHeader from 'src/components/documents/DocumentsWorkspaceHeader.vue'
import ReplaceDocumentDialog from 'src/components/documents/ReplaceDocumentDialog.vue'
import DeleteConfirmDialog from 'src/components/shell/DeleteConfirmDialog.vue'
import { downloadDocumentExtractedText } from 'src/services/api/documents'
import { useDocumentsStore } from 'src/stores/documents'
import { useShellStore } from 'src/stores/shell'

const documentsStore = useDocumentsStore()
const shellStore = useShellStore()
const headerRef = ref<InstanceType<typeof DocumentsWorkspaceHeader> | null>(null)
const downloadingId = ref<string | null>(null)
const removeDialogDocumentId = ref<string | null>(null)
const removeLoading = ref(false)
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
    if (!libraryId) return
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
    if (intervalMs <= 0) return
    refreshTimer = window.setInterval(() => {
      void documentsStore.loadWorkspace({ syncInspector: true }).catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => stopPolling())

const hasActiveFilters = computed(
  () => Boolean(workspace.value.filters.searchQuery.trim()) || workspace.value.filters.statusFilter !== '',
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
const compactPrimarySurface = computed(
  () => !showInspector.value && filteredRows.value.length > 0 && filteredRows.value.length <= 3,
)
const activeBacklogCount = computed(
  () => workspace.value.counters.queued + workspace.value.counters.processing,
)
const readyCount = computed(
  () => workspace.value.counters.ready + workspace.value.counters.readyNoGraph,
)
const removeDialogDocumentName = computed(() => {
  const documentId = removeDialogDocumentId.value
  if (!documentId) {
    return ''
  }
  if (detail.value?.id === documentId) {
    return detail.value.fileName
  }
  return workspace.value.rows.find((row) => row.id === documentId)?.fileName ?? documentId
})

async function openInGraph(graphNodeId: string) {
  await router.push({ path: '/graph', query: { node: graphNodeId } })
}

async function downloadText(id: string) {
  downloadingId.value = id
  try {
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
  } catch (e) {
    console.error('Download failed:', e)
  } finally {
    downloadingId.value = null
  }
}

async function submitAppend(content: string) {
  if (!appendDialogDocumentId.value) return
  await documentsStore.submitAppendDocument(appendDialogDocumentId.value, content)
}

async function submitReplace(file: File) {
  if (!replaceDialogDocumentId.value) return
  await documentsStore.submitReplaceDocument(replaceDialogDocumentId.value, file)
}

function requestRemove(id: string): void {
  removeDialogDocumentId.value = id
}

function closeRemoveDialog(): void {
  if (removeLoading.value) {
    return
  }
  removeDialogDocumentId.value = null
}

async function confirmRemove(): Promise<void> {
  if (!removeDialogDocumentId.value) {
    return
  }
  removeLoading.value = true
  try {
    await documentsStore.removeDocument(removeDialogDocumentId.value)
    removeDialogDocumentId.value = null
  } finally {
    removeLoading.value = false
  }
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
  <div class="rr-docs-page">
    <DocumentsWorkspaceHeader
      ref="headerRef"
      :accepted-formats="workspace.acceptedFormats"
      :max-size-mb="workspace.maxSizeMb"
      :loading="workspace.uploadInProgress"
      :total-count="workspace.rows.length"
      :active-count="activeBacklogCount"
      :failed-count="workspace.counters.failed"
      :ready-count="readyCount"
      :cost-summary="workspace.costSummary"
      :upload-failures="workspace.uploadFailures"
      :has-documents="hasDocuments"
      @select="documentsStore.uploadFiles"
      @clear-failures="documentsStore.clearUploadFailures"
    />

      <section
        v-if="!workspace.error || hasDocuments"
        class="rr-docs-page__workspace"
        :class="{ 'has-inspector': showInspector }"
      >
        <div
          class="rr-docs-page__primary"
          :class="{ 'is-sparse': compactPrimarySurface }"
        >
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
            :sort-field="workspace.filters.sortField"
            :sort-direction="workspace.filters.sortDirection"
            @detail="documentsStore.openDetail"
            @retry="documentsStore.retryDocument"
            @sort="documentsStore.toggleSort"
          />

          <DocumentsEmptyState
            v-else
            :loading="workspace.loading"
            :has-documents="hasDocuments"
            :has-active-filters="hasActiveFilters"
            @upload="triggerUpload"
            @clear-filters="clearFilters"
          />
        </div>

        <div
          v-if="showInspector"
          class="rr-docs-page__inspector"
          :class="{ 'is-open': showInspector }"
        >
          <DocumentInspectorPane
            :open="detailOpen"
            :detail="detail"
            :loading="detailLoading"
            :error="detailError"
            :downloading-id="downloadingId"
            @close="documentsStore.closeDetail"
            @append="documentsStore.openAppendDialog"
            @replace="documentsStore.openReplaceDialog"
            @retry="documentsStore.retryDocument"
            @remove="requestRemove"
            @open-in-graph="openInGraph"
            @download-text="downloadText"
          />
        </div>

        <button
          v-if="showInspector"
          type="button"
          class="rr-docs-page__backdrop"
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

  <DeleteConfirmDialog
    :open="Boolean(removeDialogDocumentId)"
    :title="$t('documents.dialogs.delete.title')"
    :target-name="removeDialogDocumentName"
    :warning="$t('documents.dialogs.delete.warning')"
    :confirm-label="$t('documents.actions.remove')"
    :loading="removeLoading"
    @close="closeRemoveDialog"
    @confirm="confirmRemove"
  />
</template>

<style scoped lang="scss">
.rr-docs-page {
  display: grid;
  gap: 18px;
  width: 100%;
  max-width: min(2360px, calc(100vw - 40px));
  margin: 0 auto;
  padding: 0 12px 16px;
}

.rr-docs-page__workspace {
  position: relative;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 16px;
  align-items: start;
}

.rr-docs-page__workspace.has-inspector {
  grid-template-columns: minmax(0, 1.32fr) minmax(440px, 0.68fr);
}

.rr-docs-page__primary {
  --rr-docs-sticky-top: 4.85rem;
  display: grid;
  gap: 0;
  min-width: 0;
  min-height: calc(100vh - 14rem);
  padding: 14px;
  border: 1px solid var(--rr-border-soft, #e2e8f0);
  border-radius: 18px;
  background: #fff;
  box-shadow: 0 14px 36px rgba(15, 23, 42, 0.05);
}

.rr-docs-page__primary.is-sparse {
  min-height: 0;
  padding-block: 9px 10px;
}

.rr-docs-page__inspector {
  position: relative;
  align-self: start;
  min-width: 0;
}

.rr-docs-page__inspector :deep(.rr-document-inspector) {
  position: sticky;
  top: 5.7rem;
  min-height: calc(100vh - 8.7rem);
  max-height: calc(100vh - 8.7rem);
  overflow: auto;
}

.rr-docs-page__backdrop {
  display: none;
  border: 0;
  padding: 0;
}

@media (min-width: 1800px) {
  .rr-docs-page {
    gap: 22px;
    max-width: min(2960px, calc(100vw - 72px));
    padding-inline: 18px;
  }

  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1.48fr) minmax(560px, 0.52fr);
    gap: 20px;
  }

  .rr-docs-page__primary {
    --rr-docs-sticky-top: 5.05rem;
    padding: 18px;
    border-radius: 20px;
  }

  .rr-docs-page__inspector :deep(.rr-document-inspector) {
    top: 5.9rem;
    min-height: calc(100vh - 9.2rem);
    max-height: calc(100vh - 9.2rem);
  }
}

@media (min-width: 2600px) {
  .rr-docs-page {
    max-width: min(3200px, calc(100vw - 104px));
  }

  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1.54fr) minmax(620px, 0.46fr);
    gap: 22px;
  }

  .rr-docs-page__primary {
    --rr-docs-sticky-top: 5.2rem;
    padding: 20px;
    border-radius: 22px;
  }
}

@media (max-width: 1280px) {
  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1fr) minmax(360px, 0.62fr);
    gap: 14px;
  }
}

@media (max-width: 980px) {
  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: 1fr;
  }

  .rr-docs-page__primary,
  .rr-docs-page__inspector :deep(.rr-document-inspector) {
    min-height: 28rem;
    max-height: none;
  }

  .rr-docs-page__backdrop {
    position: fixed;
    inset: 0;
    z-index: 19;
    display: block;
    background: rgba(15, 23, 42, 0.12);
  }

  .rr-docs-page__inspector {
    position: fixed;
    inset: auto 1rem 1rem 1rem;
    top: 5.05rem;
    z-index: 20;
    opacity: 0;
    pointer-events: none;
    transform: translateY(1rem);
    transition: opacity 0.18s ease, transform 0.18s ease;
  }

  .rr-docs-page__inspector.is-open {
    opacity: 1;
    pointer-events: auto;
    transform: translateY(0);
  }

  .rr-docs-page__inspector:not(.is-open) {
    display: none;
  }
}

@media (max-width: 820px) {
  .rr-docs-page {
    gap: 14px;
    max-width: min(100%, calc(100vw - 24px));
    padding-inline: 6px;
  }

  .rr-docs-page__primary {
    min-height: 0;
    padding: 12px;
    border-radius: 16px;
  }

  .rr-docs-page__primary.is-sparse {
    min-height: 0;
    padding-block: 12px;
  }

  .rr-docs-page__backdrop {
    background: rgba(15, 23, 42, 0.16);
  }

  .rr-docs-page__inspector {
    inset: auto 0.75rem 0.75rem 0.75rem;
    top: auto;
    transform: translateY(1.25rem);
  }

  .rr-docs-page__inspector :deep(.rr-document-inspector) {
    min-height: 0;
    max-height: min(70vh, 38rem);
    border-radius: 20px 20px 16px 16px;
  }
}
</style>
