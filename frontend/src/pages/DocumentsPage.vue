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
import type { DocumentStatus, DocumentUploadFailure } from 'src/models/ui/documents'
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
  uploadFailures,
  uploadLoading,
} =
  storeToRefs(documentsStore)
let refreshTimer: number | null = null

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

function formatDuration(value: number | null): string {
  if (value === null) {
    return '—'
  }
  const totalSeconds = Math.max(0, Math.round(value / 1000))
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  if (minutes >= 60) {
    const hours = Math.floor(minutes / 60)
    const restMinutes = minutes % 60
    return `${String(hours)}h ${String(restMinutes)}m`
  }
  if (minutes > 0) {
    return `${String(minutes)}m ${String(seconds)}s`
  }
  return `${String(totalSeconds)}s`
}

function stageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  const translated = t(key)
  return translated === key ? stage : translated
}

function accountingTone(status: string): DocumentStatus {
  switch (status) {
    case 'priced':
      return 'ready'
    case 'in_flight_unsettled':
      return 'processing'
    case 'partial':
      return 'ready_no_graph'
    default:
      return 'failed'
  }
}

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

const accountingCards = computed<{ tone: DocumentStatus; value: string; label: string }[]>(() => {
  if (!surface.value) {
    return []
  }
  const { accounting } = surface.value
  return [
    {
      tone: accountingTone(accounting.accountingStatus),
      value: formatMoney(accounting.totalEstimatedCost, accounting.currency),
      label: t('documents.collectionAccounting.totalCost'),
    },
    {
      tone: 'ready',
      value: formatMoney(accounting.settledEstimatedCost, accounting.currency),
      label: t('documents.collectionAccounting.settledCost'),
    },
    {
      tone: 'processing',
      value: formatMoney(accounting.inFlightEstimatedCost, accounting.currency),
      label: t('documents.collectionAccounting.inFlightCost'),
    },
  ]
})

const progressCards = computed(() => {
  const progress = surface.value?.diagnostics.progress
  if (!progress) {
    return []
  }
  return [
    { label: t('documents.diagnostics.progress.accepted'), value: progress.accepted },
    {
      label: t('documents.diagnostics.progress.contentExtracted'),
      value: progress.contentExtracted,
    },
    { label: t('documents.diagnostics.progress.chunked'), value: progress.chunked },
    { label: t('documents.diagnostics.progress.embedded'), value: progress.embedded },
    {
      label: t('documents.diagnostics.progress.extractingGraph'),
      value: progress.extractingGraph,
    },
    { label: t('documents.diagnostics.progress.graphReady'), value: progress.graphReady },
  ]
})

const stageDiagnostics = computed(() =>
  (surface.value?.diagnostics.perStage ?? []).filter(
    (stage) => stage.activeCount > 0 || stage.completedCount > 0 || stage.failedCount > 0,
  ),
)

const formatDiagnostics = computed(() =>
  [...(surface.value?.diagnostics.perFormat ?? [])].sort((left, right) => {
    const leftValue = left.bottleneckAvgElapsedMs ?? -1
    const rightValue = right.bottleneckAvgElapsedMs ?? -1
    return rightValue - leftValue
  }),
)

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
  const activeCount = surface.value.diagnostics.activeBacklogCount
  const extractedOnlyCount = Math.max(
    0,
    surface.value.diagnostics.progress.contentExtracted - surface.value.diagnostics.progress.chunked,
  )
  if (activeCount > 0 && rebuildBacklogCount > 0) {
    return t('documents.importGuide.activeWithBacklog', {
      count: activeCount,
      backlog: rebuildBacklogCount,
    })
  }
  if (extractedOnlyCount > 0) {
    return t('documents.importGuide.extractedOnly', { count: extractedOnlyCount })
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

const accountingBannerMessage = computed(() => {
  const accounting = surface.value?.accounting
  if (!accounting) {
    return null
  }
  if (accounting.inFlightEstimatedCost !== null || accounting.inFlightStageCount > 0) {
    return t('documents.collectionAccounting.inFlightBanner', {
      cost: formatMoney(accounting.inFlightEstimatedCost, accounting.currency),
      count: accounting.inFlightStageCount,
    })
  }
  if (accounting.missingStageCount > 0) {
    return t('documents.collectionAccounting.missingBanner', {
      count: accounting.missingStageCount,
    })
  }
  return null
})

const uploadFailureSummary = computed(() => {
  const count = uploadFailures.value.length
  if (count === 0) {
    return null
  }
  return t('documents.uploadReport.summary', { count })
})

function uploadFailureMeta(failure: DocumentUploadFailure): string[] {
  const meta: string[] = []
  if (failure.detectedFormat) {
    meta.push(`${t('documents.uploadReport.labels.format')}: ${failure.detectedFormat}`)
  }
  if (failure.mimeType) {
    meta.push(`${t('documents.uploadReport.labels.mimeType')}: ${failure.mimeType}`)
  }
  if (failure.uploadLimitMb !== null) {
    meta.push(`${t('documents.uploadReport.labels.limit')}: ${String(failure.uploadLimitMb)} MB`)
  }
  return meta
}

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

      <section
        v-if="uploadFailures.length"
        class="rr-documents__upload-report"
        role="status"
        aria-live="polite"
      >
        <div class="rr-documents__upload-report-header">
          <div>
            <strong>{{ $t('documents.uploadReport.title') }}</strong>
            <p>{{ uploadFailureSummary }}</p>
          </div>
          <button
            type="button"
            class="rr-button rr-button--ghost rr-button--tiny"
            @click="documentsStore.clearUploadFailures"
          >
            {{ $t('documents.uploadReport.dismiss') }}
          </button>
        </div>

        <ul class="rr-documents__upload-report-list">
          <li
            v-for="failure in uploadFailures"
            :key="`${failure.fileName}:${failure.message}`"
            class="rr-documents__upload-report-item"
          >
            <div class="rr-documents__upload-report-headline">
              <strong>{{ failure.fileName }}</strong>
              <span>{{ failure.message }}</span>
            </div>
            <p
              v-if="failure.rejectionCause"
              class="rr-documents__upload-report-copy"
            >
              <span>{{ $t('documents.uploadReport.labels.reason') }}:</span>
              {{ failure.rejectionCause }}
            </p>
            <p
              v-if="failure.operatorAction"
              class="rr-documents__upload-report-copy"
            >
              <span>{{ $t('documents.uploadReport.labels.action') }}:</span>
              {{ failure.operatorAction }}
            </p>
            <p
              v-if="uploadFailureMeta(failure).length"
              class="rr-documents__upload-report-meta"
            >
              {{ uploadFailureMeta(failure).join(' · ') }}
            </p>
          </li>
        </ul>
      </section>

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
        v-if="accountingBannerMessage"
        class="rr-documents__graph-banner is-progress"
      >
        <strong>{{ $t('documents.collectionAccounting.title') }}</strong>
        <p>{{ accountingBannerMessage }}</p>
      </section>

      <section
        v-if="surface?.graphWarning || graphBannerMessage"
        class="rr-documents__graph-banner"
        :class="`is-${surface?.graphStatus ?? 'empty'}`"
      >
        <strong>{{ $t(`graph.statuses.${surface?.graphStatus ?? 'empty'}`) }}</strong>
        <p>{{ graphBannerMessage }}</p>
      </section>

      <section
        v-if="accountingCards.length || progressCards.length || surface?.diagnostics"
        class="rr-documents__insights"
      >
        <details
          v-if="accountingCards.length"
          class="rr-page-card rr-documents__insight-section"
        >
          <summary>
            <strong>{{ $t('documents.collectionAccounting.title') }}</strong>
            <span>{{ $t('documents.collectionAccounting.inFlightCost') }}</span>
          </summary>
          <div class="rr-documents__summary rr-documents__summary--compact">
            <article
              v-for="card in accountingCards"
              :key="card.label"
            >
              <DocumentSummaryCard
                :tone="card.tone"
                :value="card.value"
                :label="card.label"
              />
            </article>
          </div>
        </details>

        <details
          v-if="progressCards.length"
          class="rr-page-card rr-documents__insight-section"
        >
          <summary>
            <strong>{{ $t('documents.importGuide.title') }}</strong>
            <span>{{ $t('documents.diagnostics.progress.graphReady') }}</span>
          </summary>
          <div class="rr-documents__summary rr-documents__summary--compact">
            <article
              v-for="card in progressCards"
              :key="card.label"
            >
              <DocumentSummaryCard
                tone="processing"
                :value="card.value"
                :label="card.label"
              />
            </article>
          </div>
        </details>

        <details
          v-if="surface?.diagnostics"
          class="rr-page-card rr-documents__insight-section"
        >
          <summary>
            <strong>{{ $t('documents.diagnostics.title') }}</strong>
            <span>
              {{
                $t('documents.diagnostics.backlog', {
                  queued: surface.diagnostics.queueBacklogCount,
                  processing: surface.diagnostics.processingBacklogCount,
                })
              }}
            </span>
          </summary>

          <section class="rr-documents__diagnostics">
            <div
              v-if="stageDiagnostics.length"
              class="rr-documents__diagnostics-grid"
            >
              <article
                v-for="stage in stageDiagnostics"
                :key="stage.stage"
                class="rr-documents__diagnostics-card"
              >
                <strong>{{ stageLabel(stage.stage) }}</strong>
                <p>
                  {{
                    $t('documents.diagnostics.stageSummary', {
                      active: stage.activeCount,
                      completed: stage.completedCount,
                      failed: stage.failedCount,
                    })
                  }}
                </p>
                <span>
                  {{
                    $t('documents.diagnostics.timing', {
                      avg: formatDuration(stage.avgElapsedMs),
                      max: formatDuration(stage.maxElapsedMs),
                    })
                  }}
                </span>
              </article>
            </div>

            <div
              v-if="formatDiagnostics.length"
              class="rr-documents__diagnostics-list"
            >
              <article
                v-for="format in formatDiagnostics"
                :key="format.fileType"
                class="rr-documents__diagnostics-item"
              >
                <div>
                  <strong>{{ format.fileType }}</strong>
                  <p>
                    {{
                      $t('documents.diagnostics.formatSummary', {
                        documents: format.documentCount,
                        ready: format.readyCount,
                        failed: format.failedCount,
                      })
                    }}
                  </p>
                </div>
                <div class="rr-documents__diagnostics-meta">
                  <span>
                    {{
                      $t('documents.diagnostics.queueTiming', {
                        avg: formatDuration(format.avgQueueElapsedMs),
                        total: formatDuration(format.avgTotalElapsedMs),
                      })
                    }}
                  </span>
                  <span v-if="format.bottleneckStage">
                    {{
                      $t('documents.diagnostics.bottleneck', {
                        stage: stageLabel(format.bottleneckStage),
                        avg: formatDuration(format.bottleneckAvgElapsedMs),
                      })
                    }}
                  </span>
                </div>
              </article>
            </div>
          </section>
        </details>
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
        :diagnostics="surface?.diagnostics ?? null"
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
