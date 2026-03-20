<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphLegend from 'src/components/graph/GraphLegend.vue'
import { useGraphStore } from 'src/stores/graph'
import { useShellStore } from 'src/stores/shell'

const graphStore = useGraphStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()
const {
  convergenceStatus,
  error,
  filteredArtifactCount,
  focusedNodeId,
  hasAdmittedOnlyTruth,
  isPartiallyConverged,
  layoutMode,
  loading,
  nodeTypeFilter,
  refreshIntervalMs,
  searchHits,
  searchQuery,
  showFilteredArtifacts,
  surface,
} = storeToRefs(graphStore)

let refreshTimer: number | null = null
const focusActive = ref(false)

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

const activeLibraryId = computed(() => shellStore.context?.activeLibrary.id ?? null)
const bannerVisible = computed(
  () => Boolean(surface.value) && (surface.value!.graphStatus !== 'ready' || Boolean(surface.value!.warning)),
)
const bannerDescription = computed(() => {
  if (!surface.value) {
    return null
  }
  if (surface.value.warning) {
    return surface.value.warning
  }
  return `graph.statusDescriptions.${surface.value.graphStatus}`
})

watch(
  activeLibraryId,
  async (libraryId) => {
    if (!libraryId) {
      return
    }
    await graphStore.loadSurface(libraryId)
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
      if (!activeLibraryId.value) {
        return
      }
      void graphStore.loadSurface(activeLibraryId.value, { preserveUi: true }).catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

watch(
  () => [route.query.node, surface.value?.projectionVersion] as const,
  async ([nodeId]) => {
    if (!surface.value) {
      return
    }
    if (typeof nodeId !== 'string' || !nodeId.trim()) {
      focusActive.value = false
      graphStore.clearFocus()
      return
    }

    if (focusedNodeId.value === nodeId) {
      focusActive.value = true
      return
    }

    await graphStore.focusNode(nodeId)
    focusActive.value = Boolean(graphStore.focusedNodeId)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
})

async function focusNode(id: string) {
  await graphStore.focusNode(id)
  const nextFocusedId = graphStore.focusedNodeId

  if (!nextFocusedId) {
    focusActive.value = false
    return
  }

  focusActive.value = true

  if (route.query.node !== nextFocusedId) {
    await router.replace({ query: { ...route.query, node: nextFocusedId } })
  }
}

async function selectHit(id: string) {
  await focusNode(id)
  graphStore.searchHits = []
}

async function clearFocus() {
  const nextQuery = { ...route.query }
  delete nextQuery.node
  await router.replace({ query: nextQuery })
  focusActive.value = false
  graphStore.clearFocus()
  graphStore.fitViewport()
}
</script>

<template>
  <div class="rr-graph-page">
    <ErrorStateCard
      v-if="error && !surface"
      :title="$t('graph.title')"
      :description="error"
    />

    <div
      v-else
      class="rr-graph-page__canvas-column"
    >
      <div
        v-if="surface && bannerVisible"
        class="rr-graph-page__banner"
        :class="`is-${isPartiallyConverged ? 'partial' : surface.graphStatus}`"
      >
        <strong>{{ $t(`graph.statuses.${surface.graphStatus}`) }}</strong>
        <p>{{ $t(bannerDescription ?? 'graph.statusDescriptions.default') }}</p>
        <p
          v-if="!focusedNodeId && surface.nodeCount > 120"
          class="rr-graph-page__hint"
        >
          {{ $t('graph.overviewHint') }}
        </p>
      </div>

      <section class="rr-graph-workspace">
        <div
          v-if="loading && !surface"
          class="rr-graph-page__state"
        >
          {{ $t('graph.loading') }}
        </div>

        <div
          v-else-if="surface?.graphStatus === 'building' && surface.nodeCount === 0"
          class="rr-graph-page__state"
        >
          {{ $t('graph.statusDescriptions.building') }}
        </div>

        <ErrorStateCard
          v-else-if="surface?.graphStatus === 'failed' && surface.nodeCount === 0"
          :title="$t('graph.failedTitle')"
          :description="surface.warning ?? $t('graph.failedDescription')"
        />

        <EmptyStateCard
          v-else-if="surface && surface.graphStatus === 'empty' && surface.nodeCount === 0"
          :title="$t('graph.emptyTitle')"
          :description="$t('graph.emptyDescription')"
        />

        <template v-else-if="surface">
          <GraphCanvas
            :nodes="surface.nodes"
            :edges="surface.edges"
            :filter="nodeTypeFilter"
            :focused-node-id="focusedNodeId"
            :focus-active="focusActive"
            :layout-mode="layoutMode"
            :surface-version="surface.projectionVersion"
            @select-node="focusNode"
            @clear-focus="clearFocus"
            @ready="graphStore.registerCanvasControls"
          />
          <GraphControls
            :query="searchQuery"
            :filter="nodeTypeFilter"
            :hits="searchHits"
            :layout-mode="layoutMode"
            :can-clear-focus="Boolean(focusedNodeId)"
            :graph-status="surface.graphStatus"
            :convergence-status="convergenceStatus"
            :node-count="surface.nodeCount"
            :relation-count="surface.relationCount"
            :filtered-artifact-count="filteredArtifactCount"
            :show-filtered-artifacts="showFilteredArtifacts"
            :show-status-summary="!bannerVisible"
            @zoom-in="graphStore.zoomIn"
            @zoom-out="graphStore.zoomOut"
            @fit="graphStore.fitViewport"
            @set-layout="graphStore.setLayoutMode"
            @clear-focus="clearFocus"
            @toggle-filtered-artifacts="graphStore.setShowFilteredArtifacts(!showFilteredArtifacts)"
            @update-query="graphStore.searchNodes"
            @update-filter="graphStore.setNodeTypeFilter"
            @select-hit="selectHit"
          />
          <GraphLegend
            :items="surface.legend"
            :convergence-status="convergenceStatus"
            :filtered-artifact-count="filteredArtifactCount"
            :active-provenance-only="hasAdmittedOnlyTruth"
            :show-filtered-artifacts="showFilteredArtifacts"
          />
        </template>
      </section>
    </div>
  </div>
</template>
