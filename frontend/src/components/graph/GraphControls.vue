<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type {
  GraphConvergenceStatus,
  GraphLayoutMode,
  GraphNodeType,
  GraphSearchHit,
  GraphStatus,
} from 'src/models/ui/graph'

const props = defineProps<{
  query: string
  filter: GraphNodeType | ''
  hits: GraphSearchHit[]
  layoutMode: GraphLayoutMode
  canClearFocus?: boolean
  graphStatus?: GraphStatus | null
  convergenceStatus?: GraphConvergenceStatus | null
  filteredArtifactCount?: number
  showFilteredArtifacts?: boolean
  nodeCount?: number
  edgeCount?: number
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

const { t } = useI18n()

const nodeTypeOptions = computed(
  () =>
    [
      { value: '', label: t('graph.allNodeTypes') },
      { value: 'document', label: t('graph.nodeTypes.document') },
      { value: 'entity', label: t('graph.nodeTypes.entity') },
      { value: 'topic', label: t('graph.nodeTypes.topic') },
    ] as const,
)

const showMeta = computed(
  () =>
    (props.filteredArtifactCount ?? 0) > 0 ||
    props.showFilteredArtifacts ||
    (props.nodeCount ?? 0) > 0 ||
    (props.edgeCount ?? 0) > 0 ||
    Boolean(props.graphStatus && props.graphStatus !== 'ready') ||
    Boolean(props.convergenceStatus && props.convergenceStatus !== 'current'),
)
</script>

<template>
  <div class="rr-graph-controls">
    <div class="rr-graph-controls__toolbar">
      <div class="rr-graph-controls__search-row">
        <div class="rr-graph-toolbar__search rr-graph-controls__search-shell">
          <span
            class="rr-graph-controls__field-icon"
            aria-hidden="true"
          >
            <svg
              viewBox="0 0 20 20"
              fill="none"
            >
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

        <div class="rr-graph-controls__search-actions">
          <div class="rr-graph-controls__select-shell rr-graph-controls__filter-shell">
            <span
              class="rr-graph-controls__field-icon"
              aria-hidden="true"
            >
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
                <path
                  d="M4 5h12M6.5 10h7M9 15h2"
                  stroke="currentColor"
                  stroke-linecap="round"
                  stroke-width="1.75"
                />
              </svg>
            </span>
            <select
              class="rr-graph-controls__select"
              :value="filter"
              @change="emit('updateFilter', ($event.target as HTMLSelectElement).value as GraphNodeType | '')"
            >
              <option
                v-for="option in nodeTypeOptions"
                :key="option.value || 'all'"
                :value="option.value"
              >
                {{ option.label }}
              </option>
            </select>
            <span
              class="rr-graph-controls__select-caret"
              aria-hidden="true"
            >
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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

          <div class="rr-graph-controls__select-shell rr-graph-controls__layout-shell">
            <span
              class="rr-graph-controls__field-icon"
              aria-hidden="true"
            >
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
                <path
                  d="M4 5.5h12M4 10h8M4 14.5h12"
                  stroke="currentColor"
                  stroke-linecap="round"
                  stroke-width="1.75"
                />
              </svg>
            </span>
            <select
              class="rr-graph-controls__select"
              :value="layoutMode"
              @change="emit('setLayout', ($event.target as HTMLSelectElement).value as GraphLayoutMode)"
            >
              <option value="cloud">{{ $t('graph.layouts.cloud') }}</option>
              <option value="circle">{{ $t('graph.layouts.circle') }}</option>
              <option value="rings">{{ $t('graph.layouts.rings') }}</option>
              <option value="lanes">{{ $t('graph.layouts.lanes') }}</option>
              <option value="clusters">{{ $t('graph.layouts.clusters') }}</option>
              <option value="islands">{{ $t('graph.layouts.islands') }}</option>
              <option value="spiral">{{ $t('graph.layouts.spiral') }}</option>
            </select>
            <span
              class="rr-graph-controls__select-caret"
              aria-hidden="true"
            >
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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

          <div class="rr-graph-controls__actions">
            <button
              class="rr-graph-controls__icon-button"
              type="button"
              :title="$t('graph.zoomIn')"
              :aria-label="$t('graph.zoomIn')"
              @click="emit('zoomIn')"
            >
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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
              <svg
                viewBox="0 0 20 20"
                fill="none"
              >
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

      <div
        v-if="showMeta"
        class="rr-graph-controls__meta"
      >
        <span
          v-if="(nodeCount ?? 0) > 0"
          class="rr-graph-controls__badge rr-graph-controls__badge--count"
        >
          {{ nodeCount }} {{ $t('graph.nodes') }}
        </span>
        <span
          v-if="(edgeCount ?? 0) > 0"
          class="rr-graph-controls__badge rr-graph-controls__badge--count"
        >
          {{ edgeCount }} {{ $t('graph.relations') }}
        </span>
        <button
          v-if="(filteredArtifactCount ?? 0) > 0 || showFilteredArtifacts"
          class="rr-graph-toolbar__artifact-toggle"
          :class="{ 'is-active': showFilteredArtifacts }"
          type="button"
          @click="emit('toggleFilteredArtifacts')"
        >
          <span class="rr-graph-toolbar__artifact-label">{{ $t('graph.artifacts') }}</span>
          <span class="rr-graph-toolbar__artifact-count">{{ filteredArtifactCount ?? 0 }}</span>
        </button>

        <span
          v-if="graphStatus && graphStatus !== 'ready'"
          :class="`rr-graph-controls__badge rr-graph-controls__badge--${graphStatus}`"
        >
          {{ $t(`graph.statuses.${graphStatus}`) }}
        </span>

        <span
          v-if="convergenceStatus && convergenceStatus !== 'current'"
          class="rr-graph-controls__badge rr-graph-controls__badge--convergence"
        >
          {{ $t(`graph.convergence.${convergenceStatus}`) }}
        </span>

      </div>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-graph-controls {
  border-radius: 18px;
  border: 1px solid rgba(148, 163, 184, 0.24);
  background: rgba(255, 255, 255, 0.9);
  backdrop-filter: blur(12px);
  box-shadow: 0 8px 24px rgba(15, 23, 42, 0.08);
}

.rr-graph-controls__toolbar {
  display: grid;
  gap: 0.64rem;
  padding: 0.66rem;
}

.rr-graph-controls__search-row {
  display: grid;
  gap: 0.62rem;
}

.rr-graph-controls__search-actions {
  display: flex;
  align-items: center;
  gap: 0.54rem;
  flex-wrap: wrap;
}

.rr-graph-controls__meta {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.45rem;
  padding: 0 0.66rem 0.66rem;
}

@media (max-width: 920px) {
  .rr-graph-controls {
    border-radius: 14px;
  }

  .rr-graph-controls__toolbar {
    padding: 0.48rem;
  }
}
</style>
