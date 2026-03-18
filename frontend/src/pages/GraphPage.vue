<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import EmptyStateCard from 'src/components/base/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import GraphAssistantPanel from 'src/components/graph/GraphAssistantPanel.vue'
import GraphAssistantFocusChip from 'src/components/graph/assistant/GraphAssistantFocusChip.vue'
import GraphCanvas from 'src/components/graph/GraphCanvas.vue'
import GraphControls from 'src/components/graph/GraphControls.vue'
import GraphLegend from 'src/components/graph/GraphLegend.vue'
import type { ChatFocusContext } from 'src/models/ui/chat'
import { useGraphStore } from 'src/stores/graph'
import { useShellStore } from 'src/stores/shell'

const ASSISTANT_WIDTH_STORAGE_KEY = 'rr.graph.assistant.width'
const ASSISTANT_WIDTH_DEFAULT = 520
const ASSISTANT_WIDTH_MIN = 460
const ASSISTANT_WIDTH_MAX = 760
const CANVAS_MIN_WIDTH = 560

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
  assistantSubmitting,
  assistantSettingsDraft,
  assistantSettingsOpen,
  assistantSettingsSaving,
  convergenceStatus,
  diagnostics,
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
  sessionError,
  sessionLoading,
  showFilteredArtifacts,
  sourceDisclosureState,
  surface,
} = storeToRefs(graphStore)
let refreshTimer: number | null = null
const focusActive = ref(false)
const layoutRef = ref<HTMLElement | null>(null)
const assistantColumnWidth = ref(readAssistantWidthPreference())
const assistantResizing = ref(false)

function readAssistantWidthPreference(): number {
  if (typeof window === 'undefined') {
    return ASSISTANT_WIDTH_DEFAULT
  }
  const stored = Number(window.localStorage.getItem(ASSISTANT_WIDTH_STORAGE_KEY))
  return Number.isFinite(stored) ? stored : ASSISTANT_WIDTH_DEFAULT
}

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function resolveAssistantWidthBounds(): { min: number; max: number } {
  const layoutWidth =
    layoutRef.value?.clientWidth ??
    (typeof window !== 'undefined' ? window.innerWidth - 36 : ASSISTANT_WIDTH_DEFAULT)
  const max = Math.max(
    ASSISTANT_WIDTH_MIN,
    Math.min(ASSISTANT_WIDTH_MAX, layoutWidth - CANVAS_MIN_WIDTH),
  )
  return { min: ASSISTANT_WIDTH_MIN, max }
}

function clampAssistantWidth(width: number): number {
  const { min, max } = resolveAssistantWidthBounds()
  return Math.min(Math.max(width, min), max)
}

function persistAssistantWidthPreference(width: number): void {
  if (typeof window === 'undefined') {
    return
  }
  window.localStorage.setItem(ASSISTANT_WIDTH_STORAGE_KEY, String(Math.round(width)))
}

function setAssistantWidth(width: number): void {
  const nextWidth = clampAssistantWidth(width)
  assistantColumnWidth.value = nextWidth
  persistAssistantWidthPreference(nextWidth)
}

function handleWindowResize(): void {
  assistantColumnWidth.value = clampAssistantWidth(assistantColumnWidth.value)
}

function stopAssistantResize(): void {
  if (!assistantResizing.value) {
    return
  }
  assistantResizing.value = false
  document.body.classList.remove('rr-is-resizing')
  window.removeEventListener('pointermove', handleAssistantResizeMove)
  window.removeEventListener('pointerup', stopAssistantResize)
  window.removeEventListener('pointercancel', stopAssistantResize)
}

function handleAssistantResizeMove(event: PointerEvent): void {
  if (!assistantResizing.value || !layoutRef.value) {
    return
  }
  const layoutBounds = layoutRef.value.getBoundingClientRect()
  setAssistantWidth(layoutBounds.right - event.clientX)
}

function handleAssistantResizeStart(event: PointerEvent): void {
  if (typeof window === 'undefined' || window.innerWidth <= 1180) {
    return
  }
  event.preventDefault()
  assistantResizing.value = true
  document.body.classList.add('rr-is-resizing')
  const resizeHandle = event.currentTarget as HTMLButtonElement
  resizeHandle.setPointerCapture(event.pointerId)
  window.addEventListener('pointermove', handleAssistantResizeMove)
  window.addEventListener('pointerup', stopAssistantResize)
  window.addEventListener('pointercancel', stopAssistantResize)
}

function handleAssistantResizeKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowLeft') {
    event.preventDefault()
    setAssistantWidth(assistantColumnWidth.value + 24)
  } else if (event.key === 'ArrowRight') {
    event.preventDefault()
    setAssistantWidth(assistantColumnWidth.value - 24)
  } else if (event.key === 'Home') {
    event.preventDefault()
    setAssistantWidth(ASSISTANT_WIDTH_DEFAULT)
  }
}

function resetAssistantWidth(): void {
  setAssistantWidth(ASSISTANT_WIDTH_DEFAULT)
}

const focusedSurfaceNode = computed(
  () => surface.value?.nodes.find((node) => node.id === focusedNodeId.value) ?? null,
)
const focusLabel = computed(
  () => focusedDetail.value?.label ?? focusedSurfaceNode.value?.label ?? null,
)
const assistantLayoutStyle = computed(() => ({
  '--rr-assistant-width': String(clampAssistantWidth(assistantColumnWidth.value)) + 'px',
}))
const assistantFocusContext = computed<ChatFocusContext | null>(() => {
  if (surface.value?.assistant.focusContext) {
    return surface.value.assistant.focusContext
  }
  if (!focusLabel.value && !focusedDetail.value) {
    return null
  }
  const focusSummaryCandidate = focusedDetail.value?.summary.trim()
  const focusSummary =
    focusSummaryCandidate && focusSummaryCandidate.length > 0 ? focusSummaryCandidate : null
  return {
    nodeId: focusedDetail.value?.id ?? focusedNodeId.value ?? '',
    label: focusedDetail.value?.label ?? focusLabel.value ?? '',
    summary: focusSummary ?? t('graph.selectedNodePending'),
    removable: true,
  }
})
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
  !focusedNodeId.value && (surface.value?.nodeCount ?? 0) > 120 ? t('graph.overviewHint') : null,
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
      void graphStore
        .loadSurface(activeLibraryId.value, { preserveUi: true })
        .catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
  stopAssistantResize()
  window.removeEventListener('resize', handleWindowResize)
})

onMounted(() => {
  handleWindowResize()
  window.addEventListener('resize', handleWindowResize)
})

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
      return
    }
    await graphStore.focusNode(nodeId)
  },
  { immediate: true },
)

async function selectHit(id: string) {
  await focusNode(id)
  graphStore.searchHits = []
}

async function focusNode(id: string) {
  if (focusedNodeId.value === id) {
    focusActive.value = true
    return
  }

  if (!focusedNodeId.value) {
    focusActive.value = false
  }

  await router.replace({ query: { ...route.query, node: id } })
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
      v-else-if="surface"
      ref="layoutRef"
      class="rr-graph-page__layout"
      :style="assistantLayoutStyle"
    >
      <div class="rr-graph-page__canvas-column">
        <div
          v-if="
            surface.graphStatus !== 'empty' &&
              (surface.warning ||
                diagnostics?.warning ||
                diagnostics?.lastErrorMessage ||
                diagnostics?.rebuildBacklogCount ||
                diagnostics?.readyNoGraphCount ||
                pendingDeleteBanner ||
                pendingUpdateBanner ||
                diagnostics?.lastMutationWarning ||
                convergenceBanner)
          "
          class="rr-graph-page__banner"
          :class="`is-${isPartiallyConverged ? 'partial' : surface.graphStatus}`"
        >
          <strong>{{
            convergenceBanner?.label ?? $t(`graph.statuses.${surface.graphStatus}`)
          }}</strong>
          <p>
            {{
              convergenceBanner?.description ??
                $t(`graph.statusDescriptions.${surface.graphStatus}`)
            }}
          </p>
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
            :description="
              diagnostics?.lastErrorMessage ?? surface.warning ?? $t('graph.failedDescription')
            "
          />
          <template v-else>
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
              :graph-status="surface?.graphStatus ?? diagnostics?.graphStatus ?? null"
              :convergence-status="convergenceStatus"
              :node-count="surface?.nodeCount ?? 0"
              :relation-count="surface?.relationCount ?? 0"
              :rebuild-backlog-count="diagnostics?.rebuildBacklogCount ?? 0"
              :ready-no-graph-count="diagnostics?.readyNoGraphCount ?? 0"
              :filtered-artifact-count="filteredArtifactCount"
              :show-filtered-artifacts="showFilteredArtifacts"
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

      <button
        type="button"
        class="rr-graph-page__assistant-resizer"
        :class="{ 'is-active': assistantResizing }"
        :aria-label="$t('graph.chat.resizePanel')"
        @pointerdown="handleAssistantResizeStart"
        @keydown="handleAssistantResizeKeydown"
        @dblclick="resetAssistantWidth"
      >
        <span />
      </button>

      <div class="rr-graph-page__assistant-column">
        <div class="rr-graph-page__assistant-stack">
          <GraphAssistantPanel
            :assistant="surface.assistant"
            :assistant-config="assistantConfig"
            :draft="assistantDraft"
            :error="assistantError"
            :submitting="assistantSubmitting"
            :session-loading="sessionLoading"
            :session-error="sessionError"
            :settings-open="assistantSettingsOpen"
            :settings-saving="assistantSettingsSaving"
            :settings-draft="assistantSettingsDraft"
            :source-disclosure-state="sourceDisclosureState"
            :convergence-status="convergenceStatus"
            :active-blockers="activeBlockers"
            @update-draft="graphStore.assistantDraft = $event"
            @submit="graphStore.submitAssistantPrompt"
            @select-node="focusNode"
            @create-new-chat="graphStore.createNewChat"
            @load-session="graphStore.loadChatSession"
            @open-settings="graphStore.openAssistantSettings"
            @close-settings="graphStore.closeAssistantSettings"
            @save-settings="graphStore.saveAssistantSettings"
            @restore-default-settings="graphStore.saveAssistantSettings({ restoreDefault: true })"
            @update-settings-draft-system-prompt="
              graphStore.updateAssistantSettingsDraft({ systemPrompt: $event })
            "
            @update-settings-draft-preferred-mode="
              graphStore.updateAssistantSettingsDraft({ preferredMode: $event })
            "
            @toggle-sources="graphStore.toggleMessageSources"
          />

          <GraphAssistantFocusChip
            :focus="assistantFocusContext"
            class="rr-graph-page__assistant-focus-card"
            @remove="clearFocus"
          />
        </div>
      </div>
    </div>
  </div>
</template>
