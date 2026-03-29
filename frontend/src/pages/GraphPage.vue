<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import FeedbackState from 'src/components/design-system/FeedbackState.vue'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphNodeDetailsCard from 'src/components/graph/GraphNodeDetailsCard.vue'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'
import { useGraphStore } from 'src/stores/graph'
import { useQueryStore } from 'src/stores/query'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const graphStore = useGraphStore()
const queryStore = useQueryStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()

queryStore.setGraphSurfacePriority('secondary')
const {
  convergenceStatus,
  filteredArtifactCount,
  refreshIntervalMs,
  surface,
  routeWarning,
} = storeToRefs(graphStore)

let refreshTimer: number | null = null
const isPageVisible = ref(typeof document === 'undefined' ? true : document.visibilityState === 'visible')
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
const defaultLayoutMode = computed(() =>
  resolveDefaultGraphLayoutMode(surface.value?.nodeCount ?? 0, surface.value?.edgeCount ?? 0),
)
const focusedNodeId = computed(() => inspector.value?.focusedNodeId ?? null)
const focusedNodeDetail = computed(() => inspector.value?.detail ?? null)
const focusedNodeDetailLoading = computed(() => inspector.value?.loading ?? false)
const showGraphCanvas = computed(
  () => Boolean(surface.value) && (surface.value?.nodeCount ?? 0) > 0 && canvasMode.value === 'ready',
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
    (focusedNodeDetailLoading.value || Boolean(focusedNodeDetail.value) || Boolean(inspectorError.value)),
)

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
        surface.value?.error ??
        routeWarning.value ??
        surface.value?.warning ??
        t('graph.failedDescription'),
      tone: 'failed',
    }
  }

  if (canvasMode.value === 'empty') {
    return {
      title: t('graph.emptyTitle'),
      description: t('graph.emptyDescription'),
      tone: 'empty',
    }
  }

  if (canvasMode.value === 'sparse') {
    return {
      title: t('graph.sparseTitle'),
      description: t('graph.sparseDescription'),
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

  if (overlayState.value.tone === 'sparse') {
    const details = [
      t('graph.sparseDocumentsDetail', {
        count: surface.value.nodeCount,
      }),
    ]

    if (surface.value.graphGenerationState && surface.value.graphGenerationState !== 'current') {
      details.push(
        t('graph.sparseGenerationDetail', {
          state: surface.value.graphGenerationState.replace(/_/g, ' '),
        }),
      )
    }

    return details
  }

  return []
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
    <section class="rr-graph-workbench rr-graph-workbench--immersive">
        <template v-if="showGraphCanvas">
          <GraphCanvas
            :nodes="surface.nodes"
            :edges="surface.edges"
            :filter="overlay?.nodeTypeFilter ?? ''"
            :focused-node-id="focusedNodeId"
            :layout-mode="overlay?.activeLayout ?? defaultLayoutMode"
            :show-filtered-artifacts="overlay?.showFilteredArtifacts ?? false"
            :surface-version="surface.graphGeneration"
            @select-node="focusNode"
            @clear-focus="clearFocus"
            @ready="graphStore.registerCanvasControls"
            @renderer-state="canvasRendererAvailable = $event"
          />
        </template>

        <div
          v-else
          class="rr-graph-workbench__canvas-fallback"
        />

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

        <aside
          v-if="showNodeInspector"
          class="rr-graph-workbench__inspector"
        >
          <button
            class="rr-graph-workbench__inspector-close"
            type="button"
            :aria-label="$t('graph.closeInspector')"
            :title="$t('graph.closeInspector')"
            @click="clearFocus"
          >
            <svg
              viewBox="0 0 20 20"
              fill="none"
            >
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

        <div
          v-if="overlayState"
          class="rr-graph-workbench__state"
          :class="`is-${overlayState.tone}`"
        >
          <FeedbackState
            :title="overlayState.title"
            :message="overlayState.description ?? ''"
            :details="overlayDetails"
            :kind="overlayState.tone === 'failed' ? 'error' : (overlayState.tone as 'loading' | 'empty' | 'sparse')"
            :action-label="overlayPrimaryAction?.label"
            @action="overlayPrimaryAction?.action()"
          />
        </div>

      </section>
  </div>
</template>
