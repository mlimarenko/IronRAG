<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphCoverageStateCard from 'src/components/graph/GraphCoverageStateCard.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphLoadingState from 'src/components/graph/GraphLoadingState.vue'
import GraphNodeDetailsCard from 'src/components/graph/GraphNodeDetailsCard.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'
import { useGraphStore } from 'src/stores/graph'
import { useQueryStore } from 'src/stores/query'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const { formatCompactDateTime } = useDisplayFormatters()
const graphStore = useGraphStore()
const queryStore = useQueryStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()

queryStore.setGraphSurfacePriority('secondary')
const { convergenceStatus, filteredArtifactCount, refreshIntervalMs, surface, routeWarning } =
  storeToRefs(graphStore)

let refreshTimer: number | null = null
const isPageVisible = ref(
  typeof document === 'undefined' ? true : document.visibilityState === 'visible',
)
const canvasRendererAvailable = ref(true)

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function pollGraph(): void {
  if (!activeLibraryId.value) {
    return
  }
  void graphStore.pollSurface(activeLibraryId.value).catch(() => undefined)
}

function handleVisibilityChange(): void {
  isPageVisible.value = document.visibilityState === 'visible'
}

const activeLibraryId = computed(() => shellStore.context?.activeLibrary.id ?? null)

const canvasMode = computed(() => surface.value?.canvasMode ?? 'building')
const overlay = computed(() => surface.value?.overlay ?? null)
const inspector = computed(() => surface.value?.inspector ?? null)
const readinessSummary = computed(() => surface.value?.readinessSummary ?? null)
const graphCoverage = computed(() => surface.value?.graphCoverage ?? null)
const readinessCounts = computed(
  () =>
    readinessSummary.value?.documentCountsByReadiness ?? {
      processing: 0,
      readable: 0,
      graphSparse: 0,
      graphReady: 0,
      failed: 0,
    },
)
const defaultLayoutMode = computed(() =>
  resolveDefaultGraphLayoutMode(surface.value?.nodeCount ?? 0, surface.value?.edgeCount ?? 0),
)
const activeBacklogCount = computed(() => readinessCounts.value.processing)
const readableBacklogCount = computed(
  () => readinessCounts.value.readable + readinessCounts.value.graphSparse,
)
const trackedDocumentCount = computed(
  () =>
    readinessCounts.value.processing +
    readinessCounts.value.readable +
    readinessCounts.value.graphSparse +
    readinessCounts.value.graphReady +
    readinessCounts.value.failed,
)
const focusedNodeId = computed(() => inspector.value?.focusedNodeId ?? null)
const focusedNodeDetail = computed(() => inspector.value?.detail ?? null)
const focusedNodeDetailLoading = computed(() => inspector.value?.loading ?? false)
const showGraphCanvas = computed(
  () =>
    Boolean(surface.value) && (surface.value?.nodeCount ?? 0) > 0 && canvasMode.value === 'ready',
)

const showControlDock = computed(() => {
  if (!showGraphCanvas.value) {
    return false
  }
  return canvasRendererAvailable.value
})

const inspectorError = computed(() => inspector.value?.error ?? null)

const showNodeInspector = computed(
  () =>
    canvasRendererAvailable.value &&
    Boolean(surface.value) &&
    Boolean(focusedNodeId.value) &&
    (focusedNodeDetailLoading.value ||
      Boolean(focusedNodeDetail.value) ||
      Boolean(inspectorError.value)),
)

const graphWorkbenchClasses = computed(() => ({
  'has-canvas': showGraphCanvas.value,
  'has-overlay-only': !showGraphCanvas.value && Boolean(overlayState.value),
  'has-inspector': showNodeInspector.value,
}))

const overlayState = computed(() => {
  if (!surface.value || (surface.value.loading && surface.value.nodeCount === 0)) {
    return {
      title: t('graph.title'),
      description: t('graph.loading'),
      tone: 'loading',
    }
  }

  if (canvasMode.value === 'building') {
    return {
      title: t('graph.title'),
      description: t('graph.loading'),
      tone: 'loading',
    }
  }

  if (canvasMode.value === 'error') {
    return {
      title: t('graph.failedTitle'),
      description:
        trackedDocumentCount.value > 0
          ? t('graph.failedProjectionDescription')
          : t('graph.failedDescription'),
      tone: 'failed',
    }
  }

  if (canvasMode.value === 'empty') {
    const description =
      activeBacklogCount.value > 0
        ? t('graph.emptyBuildingDescription')
        : readableBacklogCount.value > 0
          ? t('graph.emptyGraphSparseDescription')
          : t('graph.emptyDescription')

    return {
      title: t('graph.emptyTitle'),
      description,
      tone: 'empty',
    }
  }

  if (canvasMode.value === 'sparse') {
    const description =
      readableBacklogCount.value > 0
        ? t('graph.sparseGraphSparseDescription')
        : activeBacklogCount.value > 0
          ? t('graph.sparseBuildingDescription')
          : t('graph.sparseSettledDescription')

    return {
      title: t('graph.sparseTitle'),
      description,
      tone: 'sparse',
    }
  }

  return null
})

const overlayPrimaryAction = computed(() => {
  if (!overlayState.value) {
    return null
  }

  if (overlayState.value.tone === 'failed') {
    return {
      label: t('graph.retry'),
      action: () => reloadSurface(),
    }
  }

  if (overlayState.value.tone === 'empty') {
    return {
      label: t('graph.openDocuments'),
      action: () => router.push('/documents'),
    }
  }

  if (overlayState.value.tone === 'sparse') {
    return {
      label: t('graph.openDocuments'),
      action: () => router.push('/documents'),
    }
  }

  return null
})

const overlayDetails = computed(() => {
  if (!overlayState.value || !surface.value) {
    return []
  }

  if (overlayState.value.tone === 'empty') {
    const details = []

    if (trackedDocumentCount.value > 0) {
      details.push(
        t('graph.emptyTrackedDocumentsDetail', {
          count: trackedDocumentCount.value,
        }),
      )
    }

    if (activeBacklogCount.value > 0) {
      details.push(t('graph.sparseQueueDetail', { processing: readinessCounts.value.processing }))
    }

    if (readableBacklogCount.value > 0) {
      details.push(
        t('graph.sparseReadinessDetail', {
          readable: readinessCounts.value.readable,
          graphSparse: readinessCounts.value.graphSparse,
        }),
      )
    }

    return details
  }

  if (overlayState.value.tone === 'sparse') {
    const details = []

    if (surface.value.nodeCount > 0) {
      details.push(
        t('graph.sparseDocumentsDetail', {
          count: surface.value.nodeCount,
        }),
      )
    }

    if (activeBacklogCount.value > 0) {
      details.push(t('graph.sparseQueueDetail', { processing: readinessCounts.value.processing }))
    }

    if (readableBacklogCount.value > 0) {
      details.push(
        t('graph.sparseReadinessDetail', {
          readable: readinessCounts.value.readable,
          graphSparse: readinessCounts.value.graphSparse,
        }),
      )
    }

    if (surface.value.graphGenerationState && surface.value.graphGenerationState !== 'current') {
      details.push(
        t('graph.sparseGenerationDetail', {
          state: surface.value.graphGenerationState.replace(/_/g, ' '),
        }),
      )
    }

    return details
  }

  if (overlayState.value.tone === 'failed') {
    const details = []

    if (trackedDocumentCount.value > 0) {
      details.push(
        t('graph.emptyTrackedDocumentsDetail', {
          count: trackedDocumentCount.value,
        }),
      )
    }

    if (activeBacklogCount.value > 0) {
      details.push(t('graph.sparseQueueDetail', { processing: readinessCounts.value.processing }))
    }

    if (readableBacklogCount.value > 0) {
      details.push(
        t('graph.sparseReadinessDetail', {
          readable: readinessCounts.value.readable,
          graphSparse: readinessCounts.value.graphSparse,
        }),
      )
    }

    const rawError = surface.value.error?.trim() ?? routeWarning.value?.trim() ?? ''
    if (rawError && rawError.toLowerCase() !== 'internal server error') {
      details.push(rawError)
    }

    return details
  }

  return []
})

const overlayCoverageTone = computed<'loading' | 'empty' | 'sparse' | 'failed' | null>(() => {
  if (!overlayState.value) {
    return null
  }
  switch (overlayState.value.tone) {
    case 'loading':
    case 'empty':
    case 'sparse':
    case 'failed':
      return overlayState.value.tone
    default:
      return null
  }
})

const overlaySummaryMessage = computed(() => {
  if (!overlayState.value) {
    return ''
  }

  switch (overlayState.value.tone) {
    case 'loading':
      return t('graph.workbenchSummary.loading')
    case 'empty':
      return t('graph.workbenchSummary.empty')
    case 'failed':
      return t('graph.workbenchSummary.failed')
    case 'sparse':
    default:
      return t('graph.workbenchSummary.sparse')
  }
})

const overlaySummaryUpdatedAt = computed(
  () =>
    graphCoverage.value?.updatedAt ??
    readinessSummary.value?.updatedAt ??
    surface.value?.lastBuiltAt ??
    null,
)

const overlaySummaryMetrics = computed(() => {
  const convergingCount = readinessCounts.value.readable + readinessCounts.value.graphSparse
  const items = [
    {
      key: 'tracked',
      label: t('graph.workbenchSummary.metrics.tracked'),
      value: trackedDocumentCount.value,
      tone: 'default',
    },
    {
      key: 'graphReady',
      label: t('graph.workbenchSummary.metrics.graphReady'),
      value: readinessCounts.value.graphReady,
      tone: 'success',
    },
    {
      key: 'converging',
      label: t('graph.workbenchSummary.metrics.converging'),
      value: convergingCount,
      tone: convergingCount > 0 ? 'warning' : 'default',
    },
    {
      key: 'processing',
      label: t('graph.workbenchSummary.metrics.processing'),
      value: readinessCounts.value.processing,
      tone: readinessCounts.value.processing > 0 ? 'info' : 'default',
    },
    {
      key: 'failed',
      label: t('graph.workbenchSummary.metrics.failed'),
      value: readinessCounts.value.failed,
      tone: readinessCounts.value.failed > 0 ? 'danger' : 'default',
    },
  ]

  return items.filter(
    (item) => item.key === 'tracked' || item.key === 'graphReady' || Number(item.value) > 0,
  )
})

watch(
  activeLibraryId,
  async (libraryId) => {
    canvasRendererAvailable.value = true
    if (!libraryId) {
      return
    }
    try {
      await graphStore.loadSurface(libraryId)
    } catch {
      // Store error state is authoritative for page feedback.
    }
  },
  { immediate: true },
)

watch(
  [() => refreshIntervalMs.value, isPageVisible],
  ([intervalMs, pageVisible]) => {
    stopPolling()
    if (intervalMs <= 0 || !pageVisible) {
      return
    }
    refreshTimer = window.setInterval(pollGraph, intervalMs)
  },
  { immediate: true },
)

watch(isPageVisible, (pageVisible) => {
  if (!pageVisible || refreshIntervalMs.value <= 0) {
    return
  }
  pollGraph()
})

watch(
  () => [route.query.node, surface.value?.graphGeneration] as const,
  async ([nodeId]) => {
    if (!surface.value) {
      return
    }
    if (typeof nodeId !== 'string' || !nodeId.trim()) {
      graphStore.clearFocus()
      graphStore.fitViewport()
      return
    }

    if (focusedNodeId.value === nodeId) {
      return
    }

    await graphStore.focusNode(nodeId)
  },
  { immediate: true },
)

onMounted(() => {
  document.addEventListener('visibilitychange', handleVisibilityChange)
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', handleVisibilityChange)
  stopPolling()
})

async function focusNode(id: string) {
  const focusTask = graphStore.focusNode(id)
  const nextFocusedId = graphStore.surface?.inspector.focusedNodeId ?? null

  if (!nextFocusedId) {
    await focusTask
    return
  }

  if (route.query.node !== nextFocusedId) {
    await router.replace({ query: { ...route.query, node: nextFocusedId } })
  }

  await focusTask
}

async function selectHit(id: string) {
  await focusNode(id)
  graphStore.clearSearch()
}

async function clearFocus() {
  const nextQuery = { ...route.query }
  delete nextQuery.node
  await router.replace({ query: nextQuery })
  graphStore.clearFocus()
  graphStore.fitViewport()
}

async function reloadSurface() {
  if (!activeLibraryId.value) {
    return
  }

  await graphStore.loadSurface(activeLibraryId.value, { preserveUi: true })
}
</script>

<template>
  <div class="rr-graph-page rr-graph-page--immersive rr-graph-page--reset">
    <h1 class="rr-screen-reader-only">{{ $t('shell.graph') }}</h1>
    <section
      class="rr-graph-workbench rr-graph-workbench--immersive"
      :class="graphWorkbenchClasses"
    >
      <div class="rr-graph-workbench__stage">
        <template v-if="showGraphCanvas">
          <GraphCanvas
            :nodes="surface?.nodes ?? []"
            :edges="surface?.edges ?? []"
            :filter="overlay?.nodeTypeFilter ?? ''"
            :focused-node-id="focusedNodeId"
            :layout-mode="overlay?.activeLayout ?? defaultLayoutMode"
            :page-visible="isPageVisible"
            :show-filtered-artifacts="overlay?.showFilteredArtifacts ?? false"
            :surface-version="surface?.graphGeneration ?? 0"
            @select-node="focusNode"
            @clear-focus="clearFocus"
            @ready="graphStore.registerCanvasControls"
            @renderer-state="canvasRendererAvailable = $event"
          />
        </template>

        <div v-else class="rr-graph-workbench__canvas-fallback">
          <div
            v-if="overlayState"
            class="rr-graph-workbench__state"
            :class="`is-${overlayState.tone}`"
          >
            <GraphLoadingState
              v-if="overlayState.tone === 'loading'"
              :title="overlayState.title"
              :description="overlayState.description ?? ''"
              :details="overlayDetails"
              :readiness-summary="readinessSummary"
              :graph-coverage="graphCoverage"
            />
            <GraphCoverageStateCard
              v-else
              :tone="overlayCoverageTone ?? 'loading'"
              :title="overlayState.title"
              :description="overlayState.description ?? ''"
              :details="overlayDetails"
              :readiness-summary="readinessSummary"
              :graph-coverage="graphCoverage"
              :action-label="overlayPrimaryAction?.label ?? null"
              @action="overlayPrimaryAction?.action()"
            />
          </div>

          <aside v-if="overlayState" class="rr-graph-workbench__summary">
            <div class="rr-graph-workbench__summary-copy">
              <p class="rr-graph-workbench__summary-eyebrow">
                {{ t('graph.workbenchSummary.eyebrow') }}
              </p>
              <h2 class="rr-graph-workbench__summary-title">
                {{ t('graph.workbenchSummary.title') }}
              </h2>
              <p class="rr-graph-workbench__summary-body">
                {{ overlaySummaryMessage }}
              </p>
            </div>

            <div class="rr-graph-workbench__summary-metrics">
              <article
                v-for="metric in overlaySummaryMetrics"
                :key="metric.key"
                class="rr-graph-workbench__summary-metric"
                :class="`is-${metric.tone}`"
              >
                <span>{{ metric.label }}</span>
                <strong>{{ metric.value }}</strong>
              </article>
            </div>

            <p v-if="overlaySummaryUpdatedAt" class="rr-graph-workbench__summary-meta">
              {{
                t('graph.workbenchSummary.updatedAt', {
                  time: formatCompactDateTime(overlaySummaryUpdatedAt),
                })
              }}
            </p>
          </aside>
        </div>
      </div>

      <GraphControls
        v-if="showControlDock"
        class="rr-graph-workbench__controls"
        :query="overlay?.searchQuery ?? ''"
        :filter="overlay?.nodeTypeFilter ?? ''"
        :hits="overlay?.searchHits ?? []"
        :layout-mode="overlay?.activeLayout ?? defaultLayoutMode"
        :compact="showNodeInspector"
        :can-clear-focus="Boolean(focusedNodeId)"
        :graph-status="overlayState ? null : (surface?.graphStatus ?? null)"
        :convergence-status="overlayState ? null : convergenceStatus"
        :filtered-artifact-count="filteredArtifactCount"
        :show-filtered-artifacts="overlay?.showFilteredArtifacts ?? false"
        :node-count="surface?.nodeCount ?? 0"
        :edge-count="surface?.edgeCount ?? 0"
        :hidden-node-count="surface?.hiddenNodeCount ?? 0"
        @zoom-in="graphStore.zoomIn"
        @zoom-out="graphStore.zoomOut"
        @fit="graphStore.fitViewport"
        @set-layout="graphStore.setLayoutMode"
        @clear-focus="clearFocus"
        @toggle-filtered-artifacts="
          graphStore.setShowFilteredArtifacts(!(overlay?.showFilteredArtifacts ?? false))
        "
        @update-query="graphStore.searchNodes"
        @update-filter="graphStore.setNodeTypeFilter"
        @select-hit="selectHit"
      />

      <aside v-if="showNodeInspector" class="rr-graph-workbench__inspector">
        <button
          class="rr-graph-workbench__inspector-close"
          type="button"
          :aria-label="$t('graph.closeInspector')"
          :title="$t('graph.closeInspector')"
          @click="clearFocus"
        >
          <svg viewBox="0 0 20 20" fill="none">
            <path
              d="M6 6l8 8M14 6l-8 8"
              stroke="currentColor"
              stroke-linecap="round"
              stroke-width="1.8"
            />
          </svg>
        </button>
        <GraphNodeDetailsCard
          :detail="focusedNodeDetail"
          :loading="focusedNodeDetailLoading"
          :error="inspectorError"
          @select-node="focusNode"
        />
      </aside>
    </section>
  </div>
</template>
