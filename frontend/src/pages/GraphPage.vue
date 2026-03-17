<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import GraphAssistantPanel from 'src/components/graph/GraphAssistantPanel.vue'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphLegend from 'src/components/graph/GraphLegend.vue'
import GraphToolbar from 'src/components/graph/GraphToolbar.vue'
import { useGraphStore } from 'src/stores/graph'
import { useShellStore } from 'src/stores/shell'

const graphStore = useGraphStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const {
  activeBlockers,
  assistantDraft,
  assistantError,
  assistantConfig,
  assistantMode,
  assistantSubmitting,
  convergenceStatus,
  diagnostics,
  detailLoading,
  detailError,
  error,
  filteredArtifactCount,
  focusedDetail,
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

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

const focusedSurfaceNode = computed(
  () => surface.value?.nodes.find((node) => node.id === focusedNodeId.value) ?? null,
)
const focusLabel = computed(
  () => focusedDetail.value?.label ?? focusedSurfaceNode.value?.label ?? null,
)
const pendingDeleteBanner = computed(() =>
  (diagnostics.value?.pendingDeleteCount ?? 0) > 0
    ? t('graph.pendingDeleteBanner', { count: diagnostics.value?.pendingDeleteCount ?? 0 })
    : null,
)
const pendingUpdateBanner = computed(() =>
  (diagnostics.value?.pendingUpdateCount ?? 0) > 0
    ? t('graph.pendingUpdateBanner', { count: diagnostics.value?.pendingUpdateCount ?? 0 })
    : null,
)
const overviewHint = computed(() =>
  !focusedNodeId.value && (surface.value?.nodeCount ?? 0) > 120
    ? t('graph.overviewHint')
    : null,
)
const visibilityHint = computed(() =>
  showFilteredArtifacts.value
    ? t('graph.showingFilteredArtifactsHint')
    : hasAdmittedOnlyTruth.value
      ? t('graph.admittedOnlyHint')
      : null,
)
const convergenceBanner = computed(() => {
  if (!convergenceStatus.value || convergenceStatus.value === 'current') {
    return null
  }
  return {
    label: t(`graph.convergence.${convergenceStatus.value}`),
    description: t(`graph.convergenceDescriptions.${convergenceStatus.value}`),
  }
})
const activeLibraryId = computed(() => shellStore.context?.activeLibrary.id ?? null)

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

onBeforeUnmount(() => {
  stopPolling()
})

watch(
  () => [route.query.node, surface.value?.projectionVersion] as const,
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

async function selectHit(id: string) {
  await router.replace({ query: { ...route.query, node: id } })
  await graphStore.focusNode(id)
  graphStore.searchHits = []
}

async function focusNode(id: string) {
  await router.replace({ query: { ...route.query, node: id } })
  await graphStore.focusNode(id)
}

async function clearFocus() {
  const nextQuery = { ...route.query }
  delete nextQuery.node
  await router.replace({ query: nextQuery })
  graphStore.clearFocus()
  graphStore.fitViewport()
}
</script>

<template>
  <div class="rr-graph-page">
    <GraphToolbar
      :query="searchQuery"
      :filter="nodeTypeFilter"
      :hits="searchHits"
      :graph-status="surface?.graphStatus ?? diagnostics?.graphStatus ?? null"
      :convergence-status="convergenceStatus"
      :node-count="surface?.nodeCount ?? 0"
      :relation-count="surface?.relationCount ?? 0"
      :rebuild-backlog-count="diagnostics?.rebuildBacklogCount ?? 0"
      :ready-no-graph-count="diagnostics?.readyNoGraphCount ?? 0"
      :filtered-artifact-count="filteredArtifactCount"
      :focus-label="focusLabel"
      :show-filtered-artifacts="showFilteredArtifacts"
      @update-query="graphStore.searchNodes"
      @update-filter="graphStore.setNodeTypeFilter"
      @select-hit="selectHit"
      @clear-focus="clearFocus"
      @toggle-filtered-artifacts="graphStore.setShowFilteredArtifacts(!showFilteredArtifacts)"
    />

    <ErrorStateCard
      v-if="error && !surface"
      :title="$t('graph.title')"
      :description="error"
    />

    <div
      v-else-if="surface"
      class="rr-graph-page__layout"
    >
      <div class="rr-graph-page__canvas-column">
        <div
          v-if="surface.graphStatus !== 'empty' && (surface.warning || diagnostics?.warning || diagnostics?.lastErrorMessage || diagnostics?.rebuildBacklogCount || diagnostics?.readyNoGraphCount || pendingDeleteBanner || pendingUpdateBanner || diagnostics?.lastMutationWarning || convergenceBanner || filteredArtifactCount)"
          class="rr-graph-page__banner"
          :class="`is-${isPartiallyConverged ? 'partial' : surface.graphStatus}`"
        >
          <strong>{{ convergenceBanner?.label ?? $t(`graph.statuses.${surface.graphStatus}`) }}</strong>
          <p>{{ convergenceBanner?.description ?? $t(`graph.statusDescriptions.${surface.graphStatus}`) }}</p>
          <p
            v-if="diagnostics?.lastMutationWarning"
            class="rr-graph-page__hint"
          >
            {{ diagnostics.lastMutationWarning }}
          </p>
          <p
            v-if="pendingDeleteBanner"
            class="rr-graph-page__hint"
          >
            {{ pendingDeleteBanner }}
          </p>
          <p
            v-if="pendingUpdateBanner"
            class="rr-graph-page__hint"
          >
            {{ pendingUpdateBanner }}
          </p>
          <p
            v-if="diagnostics?.rebuildBacklogCount"
            class="rr-graph-page__hint"
          >
            {{ $t('graph.rebuildBacklog', { count: diagnostics.rebuildBacklogCount }) }}
          </p>
          <p
            v-if="diagnostics?.readyNoGraphCount"
            class="rr-graph-page__hint"
          >
            {{ $t('graph.readyNoGraph', { count: diagnostics.readyNoGraphCount }) }}
          </p>
          <p
            v-if="overviewHint"
            class="rr-graph-page__hint"
          >
            {{ overviewHint }}
          </p>
          <p
            v-if="visibilityHint"
            class="rr-graph-page__hint"
          >
            {{ visibilityHint }}
          </p>
          <p
            v-if="filteredArtifactCount"
            class="rr-graph-page__hint"
          >
            {{ $t('graph.filteredArtifactsHint', { count: filteredArtifactCount }) }}
          </p>
        </div>

        <section class="rr-graph-workspace">
          <div
            v-if="loading"
            class="rr-graph-page__state"
          >
            {{ $t('graph.loading') }}
          </div>
          <EmptyStateCard
            v-if="!loading && (surface.graphStatus === 'empty' || surface.nodeCount === 0)"
            :title="$t('graph.emptyTitle')"
            :description="$t('graph.emptyDescription')"
          />
          <ErrorStateCard
            v-else-if="surface.graphStatus === 'failed' && surface.nodeCount === 0"
            :title="$t('graph.failedTitle')"
            :description="diagnostics?.lastErrorMessage ?? surface.warning ?? $t('graph.failedDescription')"
          />
          <template v-else>
            <GraphCanvas
              :nodes="surface.nodes"
              :edges="surface.edges"
              :filter="nodeTypeFilter"
              :focused-node-id="focusedNodeId"
              :layout-mode="layoutMode"
              :surface-version="surface.projectionVersion"
              @select-node="focusNode"
              @clear-focus="clearFocus"
              @ready="graphStore.registerCanvasControls"
            />
            <GraphControls
              :layout-mode="layoutMode"
              :can-clear-focus="Boolean(focusedNodeId)"
              @zoom-in="graphStore.zoomIn"
              @zoom-out="graphStore.zoomOut"
              @fit="graphStore.fitViewport"
              @set-layout="graphStore.setLayoutMode"
              @clear-focus="clearFocus"
            />
            <GraphLegend
              :items="surface.legend"
              :convergence-status="convergenceStatus"
              :filtered-artifact-count="filteredArtifactCount"
              :active-provenance-only="hasAdmittedOnlyTruth"
            />
          </template>
        </section>
      </div>

      <GraphAssistantPanel
        :assistant="surface.assistant"
        :assistant-config="assistantConfig"
        :draft="assistantDraft"
        :mode="assistantMode"
        :error="assistantError"
        :submitting="assistantSubmitting"
        :focused-node-id="focusedNodeId"
        :focused-node-label="focusLabel"
        :focused-detail="focusedDetail"
        :detail-loading="detailLoading"
        :detail-error="detailError"
        :convergence-status="convergenceStatus"
        :active-blockers="activeBlockers"
        @update-draft="graphStore.assistantDraft = $event"
        @update-mode="graphStore.setAssistantMode"
        @submit="graphStore.submitAssistantPrompt"
        @select-node="focusNode"
        @clear-focus="clearFocus"
      />
    </div>
  </div>
</template>
