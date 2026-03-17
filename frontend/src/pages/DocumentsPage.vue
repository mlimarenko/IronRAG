<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import router from 'src/router'
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import AppendDocumentDialog from 'src/components/documents/AppendDocumentDialog.vue'
import DocumentDetailsDrawer from 'src/components/documents/DocumentDetailsDrawer.vue'
import DocumentsFiltersBar from 'src/components/documents/DocumentsFiltersBar.vue'
import DocumentsTable from 'src/components/documents/DocumentsTable.vue'
import DocumentSummaryCard from 'src/components/documents/DocumentSummaryCard.vue'
import ReplaceDocumentDialog from 'src/components/documents/ReplaceDocumentDialog.vue'
import UploadDropzone from 'src/components/documents/UploadDropzone.vue'
import type { DocumentStatus } from 'src/models/ui/documents'
import { downloadDocumentExtractedText } from 'src/services/api/documents'
import { useDocumentsStore } from 'src/stores/documents'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const documentsStore = useDocumentsStore()
const shellStore = useShellStore()
const {
  appendDialogDocument,
  appendDialogDocumentId,
  detail,
  detailError,
  detailLoading,
  detailOpen,
  error,
  filteredRows,
  loading,
  mutationLoading,
  replaceDialogDocument,
  replaceDialogDocumentId,
  refreshIntervalMs,
  surface,
  uploadLoading,
} =
  storeToRefs(documentsStore)
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
    documentsStore.closeDetail()
    await documentsStore.loadSurface()
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
      void documentsStore.loadSurface({ syncDetail: detailOpen.value }).catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
})

const summaryCards = computed<{ tone: DocumentStatus; value: number; label: string }[]>(() => {
  if (!surface.value) {
    return []
  }
  return [
    { tone: 'queued', value: surface.value.counters.queued, label: t('documents.queued') },
    {
      tone: 'processing',
      value: surface.value.counters.processing,
      label: t('documents.processing'),
    },
    { tone: 'ready', value: surface.value.counters.ready, label: t('documents.ready') },
    {
      tone: 'ready_no_graph',
      value: surface.value.counters.readyNoGraph,
      label: t('documents.readyNoGraph'),
    },
    { tone: 'failed', value: surface.value.counters.failed, label: t('documents.failed') },
  ]
})

const graphBannerMessage = computed(() => {
  if (!surface.value) {
    return null
  }
  if (surface.value.rebuildBacklogCount > 0) {
    return t('graph.rebuildBacklog', { count: surface.value.rebuildBacklogCount })
  }
  if (surface.value.graphStatus === 'stale') {
    return t('graph.statusDescriptions.stale')
  }
  if (surface.value.graphStatus === 'building') {
    return t('graph.statusDescriptions.building')
  }
  if (surface.value.graphStatus === 'failed') {
    return t('graph.statusDescriptions.failed')
  }
  if (surface.value.counters.readyNoGraph > 0) {
    return t('graph.readyNoGraph', { count: surface.value.counters.readyNoGraph })
  }
  return surface.value.graphWarning
})

const importProgressMessage = computed(() => {
  if (!surface.value) {
    return null
  }
  const rebuildBacklogCount = surface.value.rebuildBacklogCount
  const readyNoGraphCount = surface.value.counters.readyNoGraph
  const activeCount = surface.value.rows.filter(
    (row) =>
      row.activityStatus === 'queued' ||
      row.activityStatus === 'active' ||
      row.activityStatus === 'blocked' ||
      row.activityStatus === 'retrying' ||
      row.activityStatus === 'stalled',
  ).length
  if (activeCount > 0 && rebuildBacklogCount > 0) {
    return t('documents.importGuide.activeWithBacklog', {
      count: activeCount,
      backlog: rebuildBacklogCount,
    })
  }
  if (activeCount > 0) {
    return t('documents.importGuide.active', { count: activeCount })
  }
  if (rebuildBacklogCount > 0) {
    return t('documents.importGuide.reconciling', { count: rebuildBacklogCount })
  }
  if (readyNoGraphCount > 0) {
    return t('documents.importGuide.readyNoGraph', { count: readyNoGraphCount })
  }
  if (surface.value.graphStatus === 'partial' || surface.value.graphStatus === 'stale') {
    return t('documents.importGuide.partial')
  }
  if (surface.value.graphStatus === 'ready' && surface.value.counters.ready > 0) {
    return t('documents.importGuide.ready')
  }
  return null
})

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
</script>

<template>
  <PageSurface wide>
    <div class="rr-documents">
      <UploadDropzone
        :accepted-formats="surface?.acceptedFormats ?? []"
        :max-size-mb="surface?.maxSizeMb ?? 50"
        :loading="uploadLoading"
        @select="documentsStore.uploadFiles"
      />

      <section class="rr-documents__summary">
        <article
          v-for="card in summaryCards"
          :key="card.label"
        >
          <DocumentSummaryCard
            :tone="card.tone"
            :value="card.value"
            :label="card.label"
          />
        </article>
      </section>

      <section
        v-if="importProgressMessage"
        class="rr-documents__graph-banner is-progress"
      >
        <strong>{{ $t('documents.importGuide.title') }}</strong>
        <p>{{ importProgressMessage }}</p>
      </section>

      <section
        v-if="surface?.graphWarning || graphBannerMessage"
        class="rr-documents__graph-banner"
        :class="`is-${surface?.graphStatus ?? 'empty'}`"
      >
        <strong>{{ $t(`graph.statuses.${surface?.graphStatus ?? 'empty'}`) }}</strong>
        <p>{{ graphBannerMessage }}</p>
      </section>

      <DocumentsFiltersBar
        :search-query="documentsStore.searchQuery"
        :status-filter="documentsStore.statusFilter"
        :accounting-filter="documentsStore.accountingFilter"
        :mutation-status-filter="documentsStore.mutationStatusFilter"
        :file-type-filter="documentsStore.fileTypeFilter"
        :status-options="surface?.filters.statuses ?? []"
        :accounting-options="surface?.filters.accountingStatuses ?? []"
        :mutation-status-options="surface?.filters.mutationStatuses ?? []"
        :file-type-options="surface?.filters.fileTypes ?? []"
        @update-search="documentsStore.setSearchQuery"
        @update-status="documentsStore.setStatusFilter"
        @update-accounting="documentsStore.setAccountingFilter"
        @update-mutation-status="documentsStore.setMutationStatusFilter"
        @update-file-type="documentsStore.setFileTypeFilter"
      />

      <ErrorStateCard
        v-if="error && !surface"
        :title="$t('shell.documents')"
        :description="error"
      />

      <DocumentsTable
        v-else-if="filteredRows.length"
        :rows="filteredRows"
        :selected-id="detailOpen ? detail?.id ?? null : null"
        @detail="documentsStore.openDetail"
        @append="documentsStore.openAppendDialog"
        @replace="documentsStore.openReplaceDialog"
        @retry="documentsStore.retryDocument"
        @remove="documentsStore.removeDocument"
      />

      <EmptyStateCard
        v-else
        :title="loading ? $t('documents.loading') : $t('shell.documents')"
        :description="loading ? $t('documents.loading') : $t('documents.empty')"
      />
    </div>

    <DocumentDetailsDrawer
      :open="detailOpen"
      :detail="detail"
      :workspace-name="shellStore.context?.activeWorkspace.name ?? null"
      :loading="detailLoading"
      :error="detailError"
      @close="documentsStore.closeDetail"
      @append="documentsStore.openAppendDialog"
      @replace="documentsStore.openReplaceDialog"
      @retry="documentsStore.retryDocument"
      @remove="documentsStore.removeDocument"
      @reprocess="documentsStore.reprocessDocument"
      @open-in-graph="openInGraph"
      @download-text="downloadText"
    />

    <AppendDocumentDialog
      :open="Boolean(appendDialogDocumentId)"
      :document-name="appendDialogDocument?.fileName ?? detail?.fileName ?? null"
      :loading="mutationLoading"
      @close="documentsStore.closeAppendDialog"
      @submit="submitAppend"
    />

    <ReplaceDocumentDialog
      :open="Boolean(replaceDialogDocumentId)"
      :document-name="replaceDialogDocument?.fileName ?? detail?.fileName ?? null"
      :accepted-formats="surface?.acceptedFormats ?? []"
      :loading="mutationLoading"
      @close="documentsStore.closeReplaceDialog"
      @submit="submitReplace"
    />
  </PageSurface>
</template>
