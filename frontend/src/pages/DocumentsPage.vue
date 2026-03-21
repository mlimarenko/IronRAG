<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import router from 'src/router'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import AppendDocumentDialog from 'src/components/documents/AppendDocumentDialog.vue'
import DocumentDetailsDrawer from 'src/components/documents/DocumentDetailsDrawer.vue'
import DocumentsDiagnosticsStrip from 'src/components/documents/DocumentsDiagnosticsStrip.vue'
import DocumentsEmptyState from 'src/components/documents/DocumentsEmptyState.vue'
import DocumentsFiltersBar from 'src/components/documents/DocumentsFiltersBar.vue'
import DocumentsNoticeStack from 'src/components/documents/DocumentsNoticeStack.vue'
import DocumentsPrimarySummary from 'src/components/documents/DocumentsPrimarySummary.vue'
import DocumentsTable from 'src/components/documents/DocumentsTable.vue'
import DocumentsWorkspaceHeader from 'src/components/documents/DocumentsWorkspaceHeader.vue'
import ReplaceDocumentDialog from 'src/components/documents/ReplaceDocumentDialog.vue'
import type { DocumentStatus } from 'src/models/ui/documents'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { downloadDocumentExtractedText } from 'src/services/api/documents'
import { useDocumentsStore } from 'src/stores/documents'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const { graphWarningLabel } = useDisplayFormatters()
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
  graphDiagnostics,
  graphHealthSnapshot,
  graphDiagnosticsRefreshIntervalMs,
  loading,
  mutationLoading,
  refreshIntervalMs,
  replaceDialogDocument,
  replaceDialogDocumentId,
  surface,
  uploadFailures,
  uploadLoading,
  workspaceNoticeGroups,
  workspacePrimarySummary,
  workspaceSecondaryDiagnostics,
} = storeToRefs(documentsStore)

const graphBackendLabel = computed(() => {
  const backend = graphDiagnostics.value?.graphBackend ?? null
  if (!backend) {
    return null
  }
  const key = `documents.workspace.backends.${backend}`
  if (t(key) !== key) {
    return t(key)
  }
  return backend
})

let refreshTimer: number | null = null
let graphDiagnosticsTimer: number | null = null

function formatMoney(value: number | null, currency: string | null): string {
  if (value === null) {
    return '—'
  }
  const normalizedCurrency = currency ?? 'USD'
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: normalizedCurrency,
      maximumFractionDigits: 6,
    }).format(value)
  } catch {
    return `${value.toFixed(6)} ${normalizedCurrency}`
  }
}

function formatTimestamp(value: string | null): string {
  if (!value) {
    return '—'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(parsed)
}

function residualReasonLabel(reason: string | null): string | null {
  if (!reason) {
    return null
  }
  const key = `documents.terminal.residualReasons.${reason}`
  return t(key) === key ? reason : t(key)
}

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function stopGraphDiagnosticsPolling() {
  if (graphDiagnosticsTimer !== null) {
    window.clearInterval(graphDiagnosticsTimer)
    graphDiagnosticsTimer = null
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

watch(
  () => graphDiagnosticsRefreshIntervalMs.value,
  (intervalMs) => {
    stopGraphDiagnosticsPolling()
    if (intervalMs <= 0) {
      return
    }
    graphDiagnosticsTimer = window.setInterval(() => {
      void documentsStore.loadGraphDiagnostics({ silent: true }).catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
  stopGraphDiagnosticsPolling()
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

const terminalBanner = computed<{
  tone: DocumentStatus
  title: string
  summary: string
  chips: string[]
} | null>(() => {
  const terminal = surface.value?.diagnostics.terminalOutcome
  if (!terminal) {
    return null
  }

  const chips = [
    terminal.queuedCount > 0
      ? t('documents.terminal.blockers.queued', { count: terminal.queuedCount })
      : null,
    terminal.processingCount > 0
      ? t('documents.terminal.blockers.processing', { count: terminal.processingCount })
      : null,
    terminal.pendingGraphCount > 0
      ? t('documents.terminal.blockers.pendingGraph', { count: terminal.pendingGraphCount })
      : null,
    terminal.failedDocumentCount > 0
      ? t('documents.terminal.blockers.failedDocuments', {
          count: terminal.failedDocumentCount,
        })
      : null,
    terminal.lastTransitionAt
      ? t('documents.terminal.blockers.settledAt', {
          value: formatTimestamp(terminal.lastTransitionAt),
        })
      : null,
  ].filter((value): value is string => Boolean(value))

  if (terminal.terminalState === 'fully_settled') {
    return {
      tone: 'ready',
      title: t('documents.terminal.title'),
      summary: t('documents.terminal.summary.fully_settled'),
      chips,
    }
  }
  if (terminal.terminalState === 'failed_with_residual_work') {
    return {
      tone: 'failed',
      title: t('documents.terminal.title'),
      summary: t('documents.terminal.summary.failed_with_residual_work', {
        reason: residualReasonLabel(terminal.residualReason) ?? '—',
      }),
      chips,
    }
  }
  return {
    tone: 'processing',
    title: t('documents.terminal.title'),
    summary: t('documents.terminal.summary.live_in_flight'),
    chips,
  }
})

const graphStatusMessage = computed(() => {
  if (graphDiagnostics.value?.warning) {
    return graphWarningLabel(graphDiagnostics.value.warning)
  }
  if (graphDiagnostics.value?.blockers.length) {
    return graphWarningLabel(graphDiagnostics.value.blockers[0] ?? null)
  }
  if (!surface.value) {
    return null
  }
  if (surface.value.rebuildBacklogCount > 0) {
    return t('graph.rebuildBacklog', { count: surface.value.rebuildBacklogCount })
  }
  if (graphDiagnostics.value?.graphStatus === 'rebuilding') {
    return t('graph.statusDescriptions.rebuilding')
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
  return graphWarningLabel(surface.value.graphWarning)
})

const graphStatusLabel = computed(() => {
  const graphStatus = graphDiagnostics.value?.graphStatus ?? surface.value?.graphStatus ?? null
  if (!graphStatus || !graphStatusMessage.value) {
    return null
  }
  return t(`graph.statuses.${graphStatus}`)
})

const graphStatus = computed(
  () => graphDiagnostics.value?.graphStatus ?? surface.value?.graphStatus ?? null,
)

const supportingLines = computed(() =>
  [
    workspacePrimarySummary.value?.terminalState
      ? t(`documents.workspace.terminalStates.${workspacePrimarySummary.value.terminalState}`)
      : null,
    surface.value?.diagnostics.activeBacklogCount
      ? t('documents.workspace.activeBacklog', {
          count: surface.value.diagnostics.activeBacklogCount,
        })
      : null,
    (surface.value
      ? surface.value.accounting.inFlightEstimatedCost !== null ||
          surface.value.accounting.inFlightStageCount > 0
      : false)
      ? t('documents.workspace.liveSpend', {
          cost: formatMoney(
            surface.value?.accounting.inFlightEstimatedCost ?? null,
            surface.value?.accounting.currency ?? null,
          ),
        })
      : null,
  ].filter((line, index, items): line is string => Boolean(line) && items.indexOf(line) === index),
)

const hasActiveFilters = computed(
  () =>
    Boolean(documentsStore.searchQuery.trim()) ||
    documentsStore.statusFilter !== '' ||
    documentsStore.accountingFilter !== '' ||
    documentsStore.mutationStatusFilter !== '' ||
    documentsStore.fileTypeFilter !== '',
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
</script>

<template>
  <PageSurface wide>
    <div class="rr-documents">
      <DocumentsWorkspaceHeader
        :accepted-formats="surface?.acceptedFormats ?? []"
        :max-size-mb="surface?.maxSizeMb ?? 50"
        :loading="uploadLoading"
        :workspace-name="shellStore.context?.activeWorkspace.name ?? null"
        :library-name="shellStore.context?.activeLibrary.name ?? null"
        :upload-failures="uploadFailures"
        @select="documentsStore.uploadFiles"
        @clear-failures="documentsStore.clearUploadFailures"
      />

      <DocumentsPrimarySummary
        v-if="
          workspacePrimarySummary ||
            summaryCards.length ||
            terminalBanner ||
            supportingLines.length
        "
        :primary-summary="workspacePrimarySummary"
        :summary-cards="summaryCards"
        :terminal-banner="terminalBanner"
        :supporting-lines="supportingLines"
      />

      <DocumentsDiagnosticsStrip
        :chips="workspaceSecondaryDiagnostics"
        :graph-backend="graphBackendLabel"
        :graph-health="graphHealthSnapshot"
        :graph-status="graphStatus"
        :graph-status-label="graphStatusLabel"
        :graph-status-message="graphStatusMessage"
      />

      <DocumentsNoticeStack
        :degraded="workspaceNoticeGroups.degraded"
        :informational="workspaceNoticeGroups.informational"
      />

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
        :title="$t('documents.workspace.title')"
        :description="error"
      />

      <DocumentsTable
        v-else-if="filteredRows.length"
        :rows="filteredRows"
        :diagnostics="surface?.diagnostics ?? null"
        :selected-id="detailOpen ? detail?.id ?? null : null"
        @detail="documentsStore.openDetail"
        @append="documentsStore.openAppendDialog"
        @replace="documentsStore.openReplaceDialog"
        @retry="documentsStore.retryDocument"
        @remove="documentsStore.removeDocument"
      />

      <DocumentsEmptyState
        v-else
        :loading="loading"
        :has-documents="Boolean(surface?.rows.length)"
        :has-active-filters="hasActiveFilters"
      />
    </div>

    <DocumentDetailsDrawer
      :open="detailOpen"
      :detail="detail"
      :graph-backend="graphBackendLabel"
      :library-diagnostics="surface?.diagnostics ?? null"
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
