<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphLegend from 'src/components/graph/GraphLegend.vue'
import GraphNodeDetailsCard from 'src/components/graph/GraphNodeDetailsCard.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { useGraphStore } from 'src/stores/graph'
import { useQueryStore } from 'src/stores/query'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const { enumLabel, graphWarningLabel, shortIdentifier } = useDisplayFormatters()
const graphStore = useGraphStore()
const queryStore = useQueryStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()
const {
  convergenceStatus,
  error,
  filteredArtifactCount,
  focusedNodeId,
  focusedNodeDetail,
  focusedNodeDetailLoading,
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
const {
  activeBundle,
  activeExecution,
  activeSession,
  error: queryError,
  loadingExecution,
  loadingSessions,
  sessions,
} = storeToRefs(queryStore)

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
    return graphWarningLabel(surface.value.warning)
  }
  return `graph.statusDescriptions.${surface.value.graphStatus}`
})

watch(
  activeLibraryId,
  async (libraryId) => {
    if (!libraryId) {
      queryStore.reset()
      return
    }
    await graphStore.loadSurface(libraryId)
    await queryStore.loadSessions(libraryId).catch(() => undefined)
    if (
      (!activeSession.value || activeSession.value.session.libraryId !== libraryId) &&
      sessions.value.length > 0
    ) {
      await queryStore.loadSession(sessions.value[0].id).catch(() => undefined)
      const latestExecution = queryStore.activeExecutions[0] ?? null
      if (latestExecution) {
        await queryStore.loadExecution(latestExecution.id).catch(() => undefined)
      }
    }
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
  () => [route.query.node, surface.value?.graphGeneration] as const,
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

function formatDate(value: string | null): string {
  if (!value) {
    return '—'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  })
}

function executionStateLabel(value: string | null): string {
  return enumLabel('graph.groundedState.executionStates', value)
}

function bundleStrategyLabel(value: string | null): string {
  return enumLabel('graph.groundedState.bundleStrategies', value)
}

function resolvedModeLabel(value: string | null): string {
  return enumLabel('graph.queryModes', value)
}

function sessionTitle(value: string | null, id: string): string {
  return value?.trim() || `${t('graph.groundedState.labels.session')} ${shortIdentifier(id)}`
}

function executionTitle(id: string): string {
  return `${t('graph.groundedState.labels.execution')} ${shortIdentifier(id)}`
}
</script>

<template>
  <PageSurface wide>
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

        <section class="rr-page-card rr-graph-page__grounding-panel">
          <header class="rr-graph-page__panel-head">
            <div>
              <h3>{{ $t('graph.groundedState.title') }}</h3>
              <p>{{ $t('graph.groundedState.subtitle') }}</p>
            </div>
            <span
              v-if="activeExecution"
              class="rr-status-pill"
            >
              {{ executionStateLabel(activeExecution.execution.executionState) }}
            </span>
          </header>

          <p
            v-if="loadingSessions || loadingExecution"
            class="rr-graph-page__grounding-empty"
          >
            {{ $t('graph.groundedState.loading') }}
          </p>
          <p
            v-else-if="queryError && !activeSession"
            class="rr-graph-page__grounding-empty"
          >
            {{ queryError }}
          </p>
          <div
            v-else-if="activeSession"
            class="rr-graph-page__grounding-grid"
          >
            <article class="rr-graph-page__grounding-card">
              <span>{{ $t('graph.groundedState.labels.session') }}</span>
              <strong>{{ sessionTitle(activeSession.session.title, activeSession.session.id) }}</strong>
              <p>
                {{ $t('graph.groundedState.labels.turns') }} {{ activeSession.turns.length }}
                ·
                {{ $t('graph.groundedState.labels.executions') }} {{ activeSession.executions.length }}
              </p>
            </article>

            <article
              v-if="activeExecution"
              class="rr-graph-page__grounding-card"
            >
              <span>{{ $t('graph.groundedState.labels.execution') }}</span>
              <strong>{{ executionTitle(activeExecution.execution.id) }}</strong>
              <p>
                {{ $t('graph.groundedState.labels.started') }} {{ formatDate(activeExecution.execution.startedAt) }}
              </p>
              <p>{{ $t('graph.groundedState.labels.bundle') }} {{ shortIdentifier(activeExecution.contextBundleId) }}</p>
            </article>

            <article
              v-if="activeBundle"
              class="rr-graph-page__grounding-card"
            >
              <span>{{ $t('graph.groundedState.labels.groundedRefs') }}</span>
              <strong>
                {{ activeBundle.chunkReferences.length }} {{ $t('graph.groundedState.labels.chunks') }}
              </strong>
              <p>
                {{ activeBundle.entityReferences.length }} {{ $t('graph.groundedState.labels.entities') }}
                ·
                {{ activeBundle.relationReferences.length }} {{ $t('graph.groundedState.labels.relations') }}
                ·
                {{ activeBundle.evidenceReferences.length }} {{ $t('graph.groundedState.labels.evidence') }}
              </p>
              <p>
                {{ $t('graph.groundedState.labels.bundleStrategy') }} {{ bundleStrategyLabel(activeBundle.bundle.bundleStrategy) }}
                ·
                {{ $t('graph.groundedState.labels.resolvedMode') }} {{ resolvedModeLabel(activeBundle.bundle.resolvedMode) }}
              </p>
            </article>
          </div>
          <p
            v-else
            class="rr-graph-page__grounding-empty"
          >
            {{ $t('graph.groundedState.empty') }}
          </p>
        </section>

        <section class="rr-graph-workspace">
          <div
            v-if="loading && !surface"
            class="rr-graph-page__state"
          >
            {{ $t('graph.loading') }}
          </div>

          <div
            v-else-if="
              (surface?.graphStatus === 'building' || surface?.graphStatus === 'rebuilding') &&
              surface.nodeCount === 0
            "
            class="rr-graph-page__state"
          >
            {{
              $t(
                surface?.graphStatus === 'rebuilding'
                  ? 'graph.statusDescriptions.rebuilding'
                  : 'graph.statusDescriptions.building',
              )
            }}
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
              :surface-version="surface.graphGeneration"
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
            <GraphNodeDetailsCard
              :detail="focusedNodeDetail"
              :loading="focusedNodeDetailLoading"
              @select-node="focusNode"
            />
          </template>
        </section>
      </div>
    </div>
  </PageSurface>
</template>
