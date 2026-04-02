<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import router from 'src/router'
import AddLinkDialog from 'src/components/documents/AddLinkDialog.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import AppendDocumentDialog from 'src/components/documents/AppendDocumentDialog.vue'
import DocumentInspectorPane from 'src/components/documents/DocumentInspectorPane.vue'
import DocumentsEmptyState from 'src/components/documents/DocumentsEmptyState.vue'
import DocumentsFiltersBar from 'src/components/documents/DocumentsFiltersBar.vue'
import DocumentsList from 'src/components/documents/DocumentsList.vue'
import DocumentsWorkspaceHeader from 'src/components/documents/DocumentsWorkspaceHeader.vue'
import ReplaceDocumentDialog from 'src/components/documents/ReplaceDocumentDialog.vue'
import WebIngestRunInspector from 'src/components/documents/WebIngestRunInspector.vue'
import DeleteConfirmDialog from 'src/components/shell/DeleteConfirmDialog.vue'
import { downloadDocumentExtractedText } from 'src/services/api/documents'
import { useDocumentsStore } from 'src/stores/documents'
import { useShellStore } from 'src/stores/shell'

const documentsStore = useDocumentsStore()
const shellStore = useShellStore()
const { activeLibrary: shellActiveLibrary, context: shellContext } = storeToRefs(shellStore)
const downloadingId = ref<string | null>(null)
const removeDialogDocumentId = ref<string | null>(null)
const removeLoading = ref(false)
const inspectorHost = ref<HTMLElement | null>(null)
const {
  mergedRows,
  filteredRows,
  refreshIntervalMs,
  workspace,
  webRunLoading,
  webRunError,
  addLinkDialogOpen,
  mutationLoading,
  mutationError,
  appendDialogDocumentId,
  replaceDialogDocumentId,
} = storeToRefs(documentsStore)

const currentLibrary = computed(
  () => shellContext.value?.activeLibrary ?? shellActiveLibrary.value ?? null,
)
const currentLibraryId = computed(() => currentLibrary.value?.id ?? null)

let refreshTimer: number | null = null
function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

watch(
  () => currentLibraryId.value,
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

onBeforeUnmount(() => {
  stopPolling()
})

const hasActiveFilters = computed(
  () =>
    Boolean(workspace.value.filters.searchQuery.trim()) ||
    workspace.value.filters.statusFilter !== '',
)
const hasDocuments = computed(() =>
  Boolean(mergedRows.value.length || workspace.value.uploadQueue.length),
)
const showWorkspaceHeader = computed(
  () => hasDocuments.value || workspace.value.uploadFailures.length > 0,
)
const detail = computed(() => workspace.value.inspector.detail)
const detailLoading = computed(() => workspace.value.inspector.loading)
const detailError = computed(() => workspace.value.inspector.error)
const detailOpen = computed(() => workspace.value.selectedDocumentId !== null)
const webRunInspector = computed(() => workspace.value.webRunInspector)
const webRunDetail = computed(() => webRunInspector.value.detail)
const webRunPages = computed(() => webRunInspector.value.pages)
const webRunLoadingState = computed(() => webRunInspector.value.loading)
const webRunErrorState = computed(() => webRunInspector.value.error)
const webRunOpen = computed(() => workspace.value.selectedWebRunId !== null)
const showDocumentInspector = computed(
  () =>
    detailOpen.value &&
    (detailLoading.value || detail.value !== null || detailError.value !== null),
)
const showWebRunInspector = computed(
  () =>
    webRunOpen.value &&
    (webRunLoadingState.value || webRunDetail.value !== null || webRunErrorState.value !== null),
)
const showInspector = computed(() => showDocumentInspector.value || showWebRunInspector.value)
const compactPrimarySurface = computed(() => !showInspector.value && filteredRows.value.length <= 3)
const compactLoadingSurface = computed(
  () => workspace.value.loading && !hasDocuments.value && !hasActiveFilters.value,
)
const emptyWorkspaceSurface = computed(
  () => !workspace.value.loading && filteredRows.value.length === 0,
)
const activeBacklogCount = computed(() => workspace.value.counters.processing)
const activeWebRuns = computed(() =>
  workspace.value.webRuns.filter((run) =>
    ['accepted', 'discovering', 'processing'].includes(run.runState),
  ),
)
const recentWebRuns = computed(() => workspace.value.webRuns.slice(0, 4))
const readableCount = computed(() => workspace.value.counters.readable)
const graphSparseCount = computed(() => workspace.value.counters.graphSparse)
const graphReadyCount = computed(() => workspace.value.counters.graphReady)
const webRunActionRunId = computed(() => workspace.value.webRunActionRunId)
const missingGraphBinding = computed(
  () =>
    currentLibrary.value?.ingestionReadiness.missingBindingPurposes.includes('extract_graph') ??
    false,
)
const detailWebRunCandidate = computed(() => {
  const provenance = detail.value?.webPageProvenance
  if (
    !provenance?.runId ||
    !provenance.candidateId ||
    webRunDetail.value?.runId !== provenance.runId
  ) {
    return null
  }
  return webRunPages.value.find((page) => page.candidateId === provenance.candidateId) ?? null
})
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

watch(
  () => [showInspector.value, detail.value?.id ?? null, webRunDetail.value?.runId ?? null] as const,
  async ([isOpen]) => {
    if (!isOpen || typeof window === 'undefined' || window.innerWidth > 900) {
      return
    }
    await nextTick()
    inspectorHost.value?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  },
)

async function openInGraph(graphNodeId: string) {
  await router.push({ path: '/graph', query: { node: graphNodeId } })
}

async function openAdminBindings(): Promise<void> {
  await router.push('/admin')
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

async function submitLinkRun(
  input: Parameters<typeof documentsStore.submitWebIngestRun>[0],
): Promise<void> {
  await documentsStore.submitWebIngestRun(input)
}

function clearFilters(): void {
  documentsStore.setSearchQuery('')
  documentsStore.setStatusFilter('')
}
</script>

<template>
  <div class="rr-docs-page" :class="{ 'has-inspector-layout': showInspector }">
    <DocumentsWorkspaceHeader
      v-if="showWorkspaceHeader"
      :accepted-formats="workspace.acceptedFormats"
      :max-size-mb="workspace.maxSizeMb"
      :loading="workspace.uploadInProgress"
      :total-count="mergedRows.length"
      :active-count="activeBacklogCount"
      :readable-count="readableCount"
      :failed-count="workspace.counters.failed"
      :graph-sparse-count="graphSparseCount"
      :graph-ready-count="graphReadyCount"
      :cost-summary="workspace.costSummary"
      :upload-failures="workspace.uploadFailures"
      :has-documents="hasDocuments"
      @select="documentsStore.uploadFiles"
      @clear-failures="documentsStore.clearUploadFailures"
      @open-add-link="documentsStore.openAddLinkDialog"
    />

    <section
      v-if="missingGraphBinding"
      class="rr-docs-page__workspace-notice"
      role="status"
      aria-live="polite"
    >
      <div class="rr-docs-page__workspace-notice-copy">
        <strong>{{ $t('documents.workspace.bindingNotice.title') }}</strong>
        <p>{{ $t('documents.workspace.bindingNotice.message') }}</p>
      </div>
      <button
        type="button"
        class="rr-button rr-button--secondary rr-button--tiny"
        @click="openAdminBindings"
      >
        {{ $t('documents.workspace.bindingNotice.action') }}
      </button>
    </section>

    <section
      v-if="!workspace.error || hasDocuments"
      class="rr-docs-page__workspace"
      :class="{ 'has-inspector': showInspector }"
    >
      <div
        class="rr-docs-page__primary"
        :class="{
          'is-sparse': compactPrimarySurface,
          'is-loading-empty': compactLoadingSurface,
          'is-empty-panel': emptyWorkspaceSurface,
        }"
      >
        <DocumentsFiltersBar
          v-if="hasDocuments || hasActiveFilters || recentWebRuns.length > 0"
          :search-query="workspace.filters.searchQuery"
          :status-filter="workspace.filters.statusFilter"
          :visible-count="filteredRows.length"
          :total-count="mergedRows.length"
          :show-meta="hasActiveFilters && filteredRows.length !== mergedRows.length"
          :active-processing-count="workspace.counters.processing"
          :active-readable-count="workspace.counters.readable"
          :active-graph-sparse-count="workspace.counters.graphSparse"
          :active-web-runs="activeWebRuns"
          :recent-web-runs="recentWebRuns"
          :web-run-action-run-id="webRunActionRunId"
          @update-search="documentsStore.setSearchQuery"
          @update-status="documentsStore.setStatusFilter"
          @open-web-run="documentsStore.openWebRun"
          @cancel-web-run="documentsStore.cancelWebRun"
        />

        <DocumentsList
          v-if="filteredRows.length"
          :rows="filteredRows"
          :selected-id="detailOpen ? (detail?.id ?? null) : null"
          :sort-field="workspace.filters.sortField"
          :sort-direction="workspace.filters.sortDirection"
          @detail="documentsStore.openDetail"
          @retry="documentsStore.retryDocument"
          @sort="documentsStore.toggleSort"
        />

        <DocumentsEmptyState
          v-else
          :loading="workspace.loading"
          :accepted-formats="workspace.acceptedFormats"
          :max-size-mb="workspace.maxSizeMb"
          :upload-loading="workspace.uploadInProgress"
          :has-documents="hasDocuments"
          :has-active-filters="hasActiveFilters"
          @select="documentsStore.uploadFiles"
          @clear-filters="clearFilters"
          @open-add-link="documentsStore.openAddLinkDialog"
        />
      </div>

      <div
        v-if="showInspector"
        ref="inspectorHost"
        class="rr-docs-page__inspector"
        :class="{ 'is-open': showInspector }"
      >
        <DocumentInspectorPane
          v-if="showDocumentInspector"
          :open="detailOpen"
          :detail="detail"
          :loading="detailLoading"
          :error="detailError"
          :downloading-id="downloadingId"
          :web-run-candidate="detailWebRunCandidate"
          @close="documentsStore.closeDetail"
          @append="documentsStore.openAppendDialog"
          @replace="documentsStore.openReplaceDialog"
          @retry="documentsStore.retryDocument"
          @remove="requestRemove"
          @open-in-graph="openInGraph"
          @download-text="downloadText"
          @open-web-run="documentsStore.openWebRun"
        />
        <WebIngestRunInspector
          v-else-if="showWebRunInspector"
          :open="webRunOpen"
          :detail="webRunDetail"
          :pages="webRunPages"
          :loading="webRunLoadingState"
          :error="webRunErrorState"
          :action-loading="webRunLoading"
          @close="documentsStore.closeWebRun"
          @cancel="documentsStore.cancelWebRun"
          @open-document="documentsStore.openDetail"
        />
      </div>

      <button
        v-if="showInspector"
        type="button"
        class="rr-docs-page__backdrop"
        :aria-label="$t('dialogs.close')"
        @click="showWebRunInspector ? documentsStore.closeWebRun() : documentsStore.closeDetail()"
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

  <AddLinkDialog
    :open="addLinkDialogOpen"
    :library-id="currentLibraryId"
    :loading="webRunLoading"
    :error="webRunError"
    :recursive-enabled="true"
    @close="documentsStore.closeAddLinkDialog"
    @submit="submitLinkRun"
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
  gap: 8px;
  width: 100%;
  max-width: min(1480px, calc(100vw - 28px));
  margin: 0 auto;
  padding: 0 2px 10px;
}

.rr-docs-page.has-inspector-layout {
  max-width: min(1820px, calc(100vw - 32px));
}

.rr-docs-page__workspace-notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 10px 12px;
  border: 1px solid rgba(245, 158, 11, 0.28);
  border-radius: 14px;
  background:
    radial-gradient(circle at top right, rgba(251, 191, 36, 0.09), transparent 30%),
    linear-gradient(180deg, rgba(255, 251, 235, 0.98), rgba(255, 247, 237, 0.97));
  box-shadow: 0 10px 22px rgba(180, 83, 9, 0.06);
}

.rr-docs-page__workspace-notice-copy {
  display: grid;
  gap: 4px;
  min-width: 0;
}

.rr-docs-page__workspace-notice-copy strong {
  color: rgba(146, 64, 14, 0.96);
  font-size: 0.86rem;
}

.rr-docs-page__workspace-notice-copy p {
  margin: 0;
  color: rgba(120, 53, 15, 0.92);
  font-size: 0.76rem;
  line-height: 1.45;
}

.rr-docs-page__workspace {
  position: relative;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 10px;
  align-items: start;
}

.rr-docs-page__workspace.has-inspector {
  grid-template-columns: minmax(0, 1fr) minmax(360px, 430px);
  gap: 12px;
}

.rr-docs-page__primary {
  --rr-docs-sticky-top: 4.85rem;
  display: grid;
  gap: 0;
  min-width: 0;
  min-height: 0;
  padding: 5px;
  border: 1px solid rgba(203, 213, 225, 0.82);
  border-radius: 14px;
  background: #fff;
  box-shadow: 0 8px 18px rgba(15, 23, 42, 0.035);
}

.rr-docs-page__primary.is-sparse {
  min-height: 0;
  padding-block: 8px 9px;
}

.rr-docs-page__primary.is-loading-empty {
  min-height: 0;
  align-content: start;
  max-width: min(52rem, 100%);
  width: 100%;
  justify-self: center;
  padding: 0;
  border-color: transparent;
  background: transparent;
  box-shadow: none;
}

.rr-docs-page__primary.is-empty-panel {
  min-height: 0;
  max-width: min(52rem, 100%);
  width: 100%;
  justify-self: center;
  padding: 0;
  overflow: hidden;
  border-color: transparent;
  background: transparent;
  box-shadow: none;
}

.rr-docs-page__primary.is-loading-empty :deep(.rr-feedback-card) {
  min-height: 180px;
}

.rr-docs-page__inspector {
  position: relative;
  align-self: start;
  min-width: 0;
}

.rr-docs-page__inspector :deep(.rr-document-inspector) {
  position: sticky;
  top: 5.7rem;
  min-height: min(38rem, calc(100vh - 8.7rem));
  max-height: calc(100vh - 8.7rem);
  overflow: auto;
}

.rr-docs-page__inspector :deep(.rr-web-run-inspector) {
  position: sticky;
  top: 5.7rem;
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
    gap: 14px;
    max-width: min(1760px, calc(100vw - 64px));
    padding-inline: 10px;
  }

  .rr-docs-page.has-inspector-layout {
    max-width: min(1960px, calc(100vw - 64px));
  }

  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1fr) minmax(390px, 460px);
    gap: 14px;
  }

  .rr-docs-page__primary {
    --rr-docs-sticky-top: 5.05rem;
    padding: 8px;
    border-radius: 16px;
  }

  .rr-docs-page__inspector :deep(.rr-document-inspector) {
    top: 5.9rem;
    min-height: min(40rem, calc(100vh - 9.2rem));
    max-height: calc(100vh - 9.2rem);
  }

  .rr-docs-page__inspector :deep(.rr-web-run-inspector) {
    top: 5.9rem;
    max-height: calc(100vh - 9.2rem);
  }
}

@media (min-width: 2600px) {
  .rr-docs-page.has-inspector-layout {
    max-width: min(1940px, calc(100vw - 80px));
  }

  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1fr) minmax(420px, 480px);
    gap: 16px;
  }

  .rr-docs-page__primary {
    --rr-docs-sticky-top: 5.2rem;
    padding: 10px;
    border-radius: 16px;
  }
}

@media (max-width: 1280px) {
  .rr-docs-page__workspace.has-inspector {
    grid-template-columns: minmax(0, 1fr) minmax(330px, 390px);
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
    inset: 7.25rem 0.85rem 0.85rem 0.85rem;
    z-index: 20;
    overflow: auto;
    opacity: 0;
    pointer-events: none;
    transform: translateY(1rem);
    transition:
      opacity 0.18s ease,
      transform 0.18s ease;
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
    padding-inline: 4px;
  }

  .rr-docs-page__workspace-notice {
    flex-direction: column;
    align-items: stretch;
    gap: 10px;
    padding: 11px 12px;
  }

  .rr-docs-page__primary {
    min-height: 0;
    padding: 10px;
    border-radius: 16px;
  }

  .rr-docs-page__primary.is-sparse {
    min-height: 0;
    padding-block: 12px;
  }

  .rr-docs-page__primary.is-loading-empty {
    min-height: 0;
  }

  .rr-docs-page__primary.is-loading-empty :deep(.rr-feedback-card) {
    min-height: 152px;
  }

  .rr-docs-page__backdrop {
    background: rgba(15, 23, 42, 0.16);
  }

  .rr-docs-page__inspector {
    inset: 8.55rem 0.6rem 0.6rem 0.6rem;
    transform: translateY(1.25rem);
  }

  .rr-docs-page__inspector :deep(.rr-document-inspector) {
    min-height: 0;
    max-height: none;
    min-height: 100%;
    border-radius: 20px 20px 16px 16px;
  }
}
</style>
