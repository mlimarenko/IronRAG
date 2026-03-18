<script setup lang="ts">
import type {
  GraphConvergenceStatus,
  GraphLayoutMode,
  GraphNodeType,
  GraphSearchHit,
  GraphStatus,
} from 'src/models/ui/graph'

defineProps<{
  query: string
  filter: GraphNodeType | ''
  hits: GraphSearchHit[]
  layoutMode: GraphLayoutMode
  canClearFocus?: boolean
  graphStatus?: GraphStatus | null
  convergenceStatus?: GraphConvergenceStatus | null
  nodeCount?: number
  relationCount?: number
  rebuildBacklogCount?: number
  readyNoGraphCount?: number
  filteredArtifactCount?: number
  showFilteredArtifacts?: boolean
}>()

const emit = defineEmits<{
  zoomIn: []
  zoomOut: []
  fit: []
  setLayout: [value: GraphLayoutMode]
  clearFocus: []
  toggleFilteredArtifacts: []
  updateQuery: [value: string]
  updateFilter: [value: GraphNodeType | '']
  selectHit: [id: string]
}>()
</script>

<template>
  <div class="rr-graph-controls">
    <div class="rr-graph-controls__toolbar">
      <div class="rr-graph-controls__primary">
        <div class="rr-graph-toolbar__search rr-graph-controls__search-shell">
          <span class="rr-graph-controls__field-icon" aria-hidden="true">
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M14.166 14.167 17.5 17.5M16.667 9.167a7.5 7.5 0 1 1-15 0 7.5 7.5 0 0 1 15 0Z"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.75"
              />
            </svg>
          </span>
          <input
            class="rr-graph-controls__field"
            :value="query"
            type="search"
            :placeholder="$t('graph.search')"
            @input="emit('updateQuery', ($event.target as HTMLInputElement).value)"
          >
          <div
            v-if="hits.length"
            class="rr-graph-toolbar__hits"
          >
            <button
              v-for="hit in hits"
              :key="hit.id"
              class="rr-graph-toolbar__hit"
              type="button"
              @click="emit('selectHit', hit.id)"
            >
              <strong>{{ hit.label }}</strong>
              <span>{{ $t(`graph.nodeTypes.${hit.nodeType}`) }}</span>
            </button>
          </div>
        </div>

        <div class="rr-graph-controls__select-shell">
          <span class="rr-graph-controls__field-icon" aria-hidden="true">
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M4.167 5.833h11.666M6.667 10h6.666M8.75 14.167h2.5"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.75"
              />
            </svg>
          </span>
          <select
            class="rr-graph-controls__select"
            :value="filter"
            @change="emit('updateFilter', ($event.target as HTMLSelectElement).value as GraphNodeType | '')"
          >
            <option value="">{{ $t('graph.allNodeTypes') }}</option>
            <option value="document">{{ $t('graph.nodeTypes.document') }}</option>
            <option value="entity">{{ $t('graph.nodeTypes.entity') }}</option>
            <option value="topic">{{ $t('graph.nodeTypes.topic') }}</option>
          </select>
          <span class="rr-graph-controls__select-caret" aria-hidden="true">
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="m5 7.5 5 5 5-5"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.75"
              />
            </svg>
          </span>
        </div>

        <div
          v-if="
            (filteredArtifactCount ?? 0) > 0 ||
              showFilteredArtifacts ||
              graphStatus ||
              convergenceStatus ||
              (nodeCount ?? 0) > 0 ||
              (relationCount ?? 0) > 0
          "
          class="rr-graph-controls__context"
        >
          <button
            v-if="(filteredArtifactCount ?? 0) > 0 || showFilteredArtifacts"
            class="rr-graph-toolbar__artifact-toggle"
            :class="{ 'is-active': showFilteredArtifacts }"
            type="button"
            @click="emit('toggleFilteredArtifacts')"
          >
            <span class="rr-graph-toolbar__artifact-label">{{ $t('graph.artifacts') }}</span>
            <span class="rr-graph-toolbar__artifact-state">
              {{
                showFilteredArtifacts
                  ? $t('graph.artifactsShown')
                  : $t('graph.artifactsHidden')
              }}
            </span>
            <span class="rr-graph-toolbar__artifact-count">{{ filteredArtifactCount ?? 0 }}</span>
          </button>

          <div class="rr-graph-toolbar__count">
            <span
              v-if="graphStatus"
              :class="`rr-graph-toolbar__status rr-graph-toolbar__status--${graphStatus}`"
            >
              {{ $t(`graph.statuses.${graphStatus}`) }}
            </span>
            <span
              v-if="convergenceStatus"
              class="rr-graph-toolbar__status rr-graph-toolbar__status--convergence"
            >
              {{ $t(`graph.convergence.${convergenceStatus}`) }}
            </span>
            <span
              v-if="(nodeCount ?? 0) > 0"
              class="rr-graph-toolbar__metric"
            >
              {{ nodeCount }} {{ $t('graph.nodes') }}
            </span>
            <span
              v-if="(relationCount ?? 0) > 0"
              class="rr-graph-toolbar__metric"
            >
              {{ relationCount }} {{ $t('graph.relations') }}
            </span>
            <span
              v-if="rebuildBacklogCount"
              class="rr-graph-toolbar__meta rr-graph-toolbar__meta--backlog"
            >
              {{ $t('graph.toolbarBacklog', { count: rebuildBacklogCount }) }}
            </span>
            <span
              v-if="readyNoGraphCount"
              class="rr-graph-toolbar__meta rr-graph-toolbar__meta--ready-no-graph"
            >
              {{ $t('graph.toolbarReadyNoGraph', { count: readyNoGraphCount }) }}
            </span>
          </div>
        </div>
      </div>

      <div class="rr-graph-controls__secondary">
        <div class="rr-graph-controls__layouts">
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'cloud' }"
            type="button"
            @click="emit('setLayout', 'cloud')"
          >
            {{ $t('graph.layouts.cloud') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'circle' }"
            type="button"
            @click="emit('setLayout', 'circle')"
          >
            {{ $t('graph.layouts.circle') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'rings' }"
            type="button"
            @click="emit('setLayout', 'rings')"
          >
            {{ $t('graph.layouts.rings') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'lanes' }"
            type="button"
            @click="emit('setLayout', 'lanes')"
          >
            {{ $t('graph.layouts.lanes') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'clusters' }"
            type="button"
            @click="emit('setLayout', 'clusters')"
          >
            {{ $t('graph.layouts.clusters') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'islands' }"
            type="button"
            @click="emit('setLayout', 'islands')"
          >
            {{ $t('graph.layouts.islands') }}
          </button>
          <button
            class="rr-button rr-button--ghost rr-button--tiny"
            :class="{ 'is-active': layoutMode === 'spiral' }"
            type="button"
            @click="emit('setLayout', 'spiral')"
          >
            {{ $t('graph.layouts.spiral') }}
          </button>
        </div>
        <div class="rr-graph-controls__actions">
          <button
            class="rr-graph-controls__icon-button"
            type="button"
            :title="$t('graph.zoomIn')"
            :aria-label="$t('graph.zoomIn')"
            @click="emit('zoomIn')"
          >
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M10 5v10M5 10h10"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-width="1.9"
              />
            </svg>
          </button>
          <button
            class="rr-graph-controls__icon-button"
            type="button"
            :title="$t('graph.zoomOut')"
            :aria-label="$t('graph.zoomOut')"
            @click="emit('zoomOut')"
          >
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M5 10h10"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-width="1.9"
              />
            </svg>
          </button>
          <button
            class="rr-graph-controls__icon-button"
            type="button"
            :title="$t('graph.fit')"
            :aria-label="$t('graph.fit')"
            @click="emit('fit')"
          >
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M7 3.5H3.5V7M13 3.5h3.5V7M16.5 13V16.5H13M7 16.5H3.5V13"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.65"
              />
              <path
                d="m7.25 7.25-3.5-3.5M12.75 7.25l3.5-3.5M7.25 12.75l-3.5 3.5M12.75 12.75l3.5 3.5"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-width="1.65"
              />
            </svg>
          </button>
          <button
            v-if="canClearFocus"
            class="rr-graph-controls__icon-button"
            type="button"
            :title="$t('graph.clearFocus')"
            :aria-label="$t('graph.clearFocus')"
            @click="emit('clearFocus')"
          >
            <svg viewBox="0 0 20 20" fill="none">
              <path
                d="M6 6l8 8M14 6l-8 8"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-width="1.9"
              />
            </svg>
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
