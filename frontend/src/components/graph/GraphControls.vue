<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'
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
  compact?: boolean
  canClearFocus?: boolean
  graphStatus?: GraphStatus | null
  convergenceStatus?: GraphConvergenceStatus | null
  filteredArtifactCount?: number
  showFilteredArtifacts?: boolean
  nodeCount?: number
  edgeCount?: number
  hiddenNodeCount?: number
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

function onSearchInput(event: Event) {
  emit('updateQuery', (event.target as HTMLInputElement).value)
}

function clearSearch() {
  emit('updateQuery', '')
}

const trimmedQuery = computed(() => props.query.trim())
const searchActive = computed(() => trimmedQuery.value.length > 0)

const showMeta = computed(
  () =>
    !searchActive.value &&
    ((props.filteredArtifactCount ?? 0) > 0 ||
      props.showFilteredArtifacts ||
      (props.nodeCount ?? 0) > 0 ||
      (props.edgeCount ?? 0) > 0 ||
      Boolean(props.graphStatus && props.graphStatus !== 'ready') ||
      Boolean(props.convergenceStatus && props.convergenceStatus !== 'current')),
)

const denseGraphHint = computed(() => {
  const nodeCount = props.nodeCount ?? 0
  const edgeCount = props.edgeCount ?? 0
  const recommendedLayout = resolveDefaultGraphLayoutMode(nodeCount, edgeCount)

  if (nodeCount === 0 || recommendedLayout === 'cloud') {
    return null
  }

  const layoutLabel = t(`graph.layouts.${recommendedLayout}`)

  if (props.layoutMode === recommendedLayout) {
    return t('graph.firstViewGuidance', { layout: layoutLabel })
  }

  return t('graph.firstViewSuggestion', { layout: layoutLabel })
})

const recommendedLayoutMode = computed(() =>
  resolveDefaultGraphLayoutMode(props.nodeCount ?? 0, props.edgeCount ?? 0),
)
const summaryHintText = computed(() => {
  if (!denseGraphHint.value || props.compact || props.layoutMode === recommendedLayoutMode.value) {
    return null
  }
  return denseGraphHint.value
})

const summaryCountBadges = computed(() =>
  [
    (props.nodeCount ?? 0) > 0 ? `${String(props.nodeCount ?? 0)} ${t('graph.nodes')}` : null,
    (props.edgeCount ?? 0) > 0 ? `${String(props.edgeCount ?? 0)} ${t('graph.relations')}` : null,
  ].filter((value): value is string => Boolean(value)),
)

const hiddenCountBadge = computed(() =>
  (props.hiddenNodeCount ?? 0) > 0
    ? t('graph.hiddenDisconnectedBadge', { count: props.hiddenNodeCount ?? 0 })
    : null,
)
const summaryCompactLine = computed(() => {
  const parts = [...summaryCountBadges.value]

  if (primaryStateBadge.value?.label) {
    parts.push(primaryStateBadge.value.label)
  }

  if (hiddenCountBadge.value) {
    parts.push(hiddenCountBadge.value)
  }

  return parts.length ? parts.join(' · ') : null
})

const showSearchResults = computed(() => searchActive.value && props.hits.length > 0)
const showSearchEmpty = computed(() => searchActive.value && props.hits.length === 0)
const searchResultSummary = computed(() =>
  t('graph.searchResultsCount', { count: props.hits.length }),
)
const primaryStateBadge = computed(() => {
  if (props.convergenceStatus && props.convergenceStatus !== 'current') {
    return {
      className: 'rr-graph-controls__badge rr-graph-controls__badge--convergence',
      label: t(`graph.convergence.${props.convergenceStatus}`),
    }
  }

  if (props.graphStatus && props.graphStatus !== 'ready') {
    return {
      className: `rr-graph-controls__badge rr-graph-controls__badge--${props.graphStatus}`,
      label: t(`graph.statuses.${props.graphStatus}`),
    }
  }

  return null
})
const showSummary = computed(
  () =>
    !searchActive.value &&
    Boolean(
      denseGraphHint.value !== null ||
      summaryCountBadges.value.length > 0 ||
      (props.filteredArtifactCount ?? 0) > 0 ||
      props.showFilteredArtifacts ||
      primaryStateBadge.value,
    ),
)
</script>

<template>
  <div class="rr-graph-controls" :class="{ 'is-compact': compact }">
    <div class="rr-graph-controls__toolbar">
      <div class="rr-graph-controls__search-row">
        <div class="rr-graph-controls__search-shell">
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
            :aria-label="$t('graph.searchNodes', 'Search nodes')"
            :aria-expanded="showSearchResults || showSearchEmpty"
            @input="onSearchInput"
          />
          <button
            v-if="trimmedQuery"
            class="rr-graph-controls__clear-button"
            type="button"
            :aria-label="$t('graph.searchClear', 'Clear search')"
            :title="$t('graph.searchClear', 'Clear search')"
            @click="clearSearch"
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
          <div v-if="showSearchResults || showSearchEmpty" class="rr-graph-controls__results">
            <template v-if="showSearchResults">
              <div class="rr-graph-controls__results-summary">
                {{ searchResultSummary }}
              </div>
              <button
                v-for="hit in hits"
                :key="hit.id"
                class="rr-graph-controls__result"
                type="button"
                @click="emit('selectHit', hit.id)"
              >
                <span class="rr-graph-controls__result-copy">
                  <strong>{{ hit.label }}</strong>
                  <span v-if="hit.preview || hit.secondaryLabel">
                    {{ hit.preview ?? hit.secondaryLabel }}
                  </span>
                </span>
                <span class="rr-graph-controls__result-type">
                  {{ $t(`graph.nodeTypes.${hit.nodeType}`) }}
                </span>
              </button>
            </template>
            <div v-else class="rr-graph-controls__search-empty">
              <strong>{{ $t('graph.searchNoMatchesTitle') }}</strong>
              <p>{{ $t('graph.searchNoMatchesBody') }}</p>
            </div>
          </div>
        </div>

        <div class="rr-graph-controls__search-actions">
          <div class="rr-graph-controls__select-shell rr-graph-controls__filter-shell">
            <span class="rr-graph-controls__field-icon" aria-hidden="true">
              <svg viewBox="0 0 20 20" fill="none">
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
              :aria-label="$t('graph.nodeTypeFilter', 'Node type filter')"
              @change="
                emit(
                  'updateFilter',
                  ($event.target as HTMLSelectElement).value as GraphNodeType | '',
                )
              "
            >
              <option
                v-for="option in nodeTypeOptions"
                :key="option.value || 'all'"
                :value="option.value"
              >
                {{ option.label }}
              </option>
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

          <div class="rr-graph-controls__select-shell rr-graph-controls__layout-shell">
            <span class="rr-graph-controls__field-icon" aria-hidden="true">
              <svg viewBox="0 0 20 20" fill="none">
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
              :aria-label="$t('graph.layoutMode', 'Layout mode')"
              @change="
                emit('setLayout', ($event.target as HTMLSelectElement).value as GraphLayoutMode)
              "
            >
              <option value="cloud">{{ $t('graph.layouts.cloud') }}</option>
              <option value="circle">{{ $t('graph.layouts.circle') }}</option>
              <option value="rings">{{ $t('graph.layouts.rings') }}</option>
              <option value="lanes">{{ $t('graph.layouts.lanes') }}</option>
              <option value="clusters">{{ $t('graph.layouts.clusters') }}</option>
              <option value="islands">{{ $t('graph.layouts.islands') }}</option>
              <option value="spiral">{{ $t('graph.layouts.spiral') }}</option>
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

      <div v-if="showSummary" class="rr-graph-controls__summary">
        <p v-if="summaryHintText" class="rr-graph-controls__hint-copy">
          {{ summaryHintText }}
        </p>
        <p v-if="summaryCompactLine" class="rr-graph-controls__summary-line">
          {{ summaryCompactLine }}
        </p>

        <div v-if="showMeta || summaryCountBadges.length" class="rr-graph-controls__meta">
          <span
            v-for="badge in summaryCountBadges"
            :key="badge"
            class="rr-graph-controls__badge rr-graph-controls__badge--count rr-graph-controls__badge--summary rr-graph-controls__badge--summary-dense"
          >
            {{ badge }}
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
            v-if="primaryStateBadge"
            :class="[primaryStateBadge.className, 'rr-graph-controls__badge--summary-dense']"
          >
            {{ primaryStateBadge.label }}
          </span>
          <span
            v-if="hiddenCountBadge"
            class="rr-graph-controls__badge rr-graph-controls__badge--summary rr-graph-controls__badge--quiet rr-graph-controls__badge--summary-dense"
          >
            {{ hiddenCountBadge }}
          </span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-graph-controls {
  overflow: visible;
  border-radius: 20px;
  border: 1px solid rgba(191, 203, 227, 0.54);
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 251, 255, 0.94)),
    rgba(255, 255, 255, 0.96);
  box-shadow:
    0 18px 34px rgba(15, 23, 42, 0.08),
    0 6px 14px rgba(15, 23, 42, 0.04);
  backdrop-filter: blur(18px);
}

.rr-graph-controls__toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-start;
  gap: 0.62rem 0.72rem;
  padding: 0.72rem 0.76rem 0.68rem;
}

.rr-graph-controls__search-shell {
  position: relative;
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  flex: 1 1 18rem;
  width: auto;
  min-width: min(100%, 16rem);
  gap: 0.6rem;
  min-height: 42px;
  padding: 0 0.82rem;
  border: 1px solid rgba(176, 190, 214, 0.44);
  border-radius: 15px;
  background: rgba(255, 255, 255, 1);
  box-shadow:
    inset 0 0 0 1px rgba(226, 232, 240, 0.82),
    0 10px 20px rgba(15, 23, 42, 0.05);
}

.rr-graph-controls__search-row {
  display: flex;
  flex: 999 1 36rem;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.62rem;
  min-width: min(100%, 24rem);
}

.rr-graph-controls__field-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  color: #7b8ba8;
  flex: 0 0 16px;
}

.rr-graph-controls__field-icon svg {
  width: 100%;
  height: 100%;
}

.rr-graph-controls__clear-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 999px;
  background: rgba(241, 245, 249, 0.92);
  color: #64748b;
  cursor: pointer;
  transition:
    background 120ms ease,
    color 120ms ease;
}

.rr-graph-controls__clear-button:hover {
  background: rgba(226, 232, 240, 0.96);
  color: #334155;
}

.rr-graph-controls__clear-button svg {
  width: 14px;
  height: 14px;
}

.rr-graph-controls__results {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  right: 0;
  z-index: 5;
  display: grid;
  gap: 6px;
  padding: 8px;
  border: 1px solid rgba(203, 213, 225, 0.94);
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.98);
  box-shadow: 0 18px 36px rgba(15, 23, 42, 0.12);
  max-height: min(22rem, calc(100vh - 11rem));
  overflow: auto;
}

.rr-graph-controls__results-summary {
  padding: 2px 6px 4px;
  color: #64748b;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.rr-graph-controls__result {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: start;
  gap: 10px;
  padding: 10px 12px;
  border: none;
  border-radius: 12px;
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition: background 120ms ease;
}

.rr-graph-controls__result:hover {
  background: rgba(241, 245, 249, 0.92);
}

.rr-graph-controls__result-copy {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.rr-graph-controls__result-copy strong {
  color: var(--rr-text-primary);
  font-size: 13px;
  font-weight: 700;
  line-height: 1.35;
}

.rr-graph-controls__result-copy span {
  color: var(--rr-text-secondary);
  font-size: 11.5px;
  line-height: 1.4;
  white-space: normal;
}

.rr-graph-controls__result-type {
  display: inline-flex;
  align-items: center;
  min-height: 22px;
  padding: 0 8px;
  border-radius: 999px;
  background: rgba(239, 244, 255, 0.96);
  color: #4f46e5;
  font-size: 11px;
  font-weight: 700;
  white-space: nowrap;
}

.rr-graph-controls__search-empty {
  display: grid;
  gap: 4px;
  padding: 12px;
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.96);
}

.rr-graph-controls__search-empty strong {
  color: var(--rr-text-primary);
  font-size: 12px;
  font-weight: 700;
}

.rr-graph-controls__search-empty p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 11.5px;
  line-height: 1.45;
}

.rr-graph-controls__search-actions {
  display: flex;
  flex: 1 1 auto;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.42rem;
  width: auto;
  min-width: min(100%, 18rem);
}

.rr-graph-controls__filter-shell,
.rr-graph-controls__layout-shell {
  flex: 0 1 auto;
  min-width: 9.5rem;
}

.rr-graph-controls__actions {
  flex: 0 0 auto;
}

.rr-graph-controls__summary {
  display: flex;
  flex: 1 1 18rem;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.44rem 0.56rem;
  min-width: min(100%, 16rem);
  padding: 0.06rem 0.08rem 0;
}

.rr-graph-controls__summary-line {
  display: none;
  margin: 0;
  padding: 0 0.08rem;
  color: rgba(71, 85, 105, 0.94);
  font-size: 0.64rem;
  font-weight: 700;
  line-height: 1.3;
}

.rr-graph-controls__hint-copy {
  margin: 0;
  flex: 1 1 20rem;
  min-width: min(100%, 16rem);
  padding: 0;
  color: rgba(100, 116, 139, 0.92);
  font-size: 0.7rem;
  font-weight: 600;
  line-height: 1.35;
}

.rr-graph-controls__meta {
  display: flex;
  flex: 999 1 auto;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.42rem;
}

.rr-graph-controls__meta-caption {
  margin: 0;
  padding: 0 0.1rem;
  color: rgba(100, 116, 139, 0.9);
  font-size: 0.66rem;
  font-weight: 600;
  line-height: 1.3;
}

.rr-graph-controls__badge--summary {
  font-weight: 700;
  color: #334155;
  max-width: 100%;
}

.rr-graph-controls__badge--quiet {
  color: rgba(71, 85, 105, 0.94);
  background: rgba(248, 250, 252, 0.98);
}

.rr-graph-controls.is-compact .rr-graph-controls__toolbar {
  gap: 0.46rem 0.54rem;
  padding: 0.58rem 0.62rem 0.56rem;
}

.rr-graph-controls.is-compact .rr-graph-controls__search-row {
  gap: 0.46rem 0.52rem;
}

.rr-graph-controls.is-compact .rr-graph-controls__search-actions {
  gap: 0.4rem;
}

.rr-graph-controls.is-compact .rr-graph-controls__hint-copy {
  font-size: 0.64rem;
  line-height: 1.28;
}

.rr-graph-controls.is-compact .rr-graph-controls__meta {
  gap: 0.34rem;
}

@media (max-width: 1180px) {
  .rr-graph-controls__toolbar {
    gap: 0.54rem 0.58rem;
    padding: 0.62rem 0.64rem 0.58rem;
  }

  .rr-graph-controls__search-shell {
    gap: 0.58rem;
    min-height: 40px;
    padding: 0 0.7rem;
    border-radius: 14px;
  }

  .rr-graph-controls__results {
    gap: 5px;
    padding: 7px;
  }

  .rr-graph-controls__result {
    padding: 9px 10px;
  }

  .rr-graph-controls__search-row {
    gap: 0.52rem;
  }

  .rr-graph-controls__search-actions {
    gap: 0.46rem;
  }

  .rr-graph-controls__summary {
    gap: 0.38rem 0.48rem;
    padding-top: 0;
  }

  .rr-graph-controls__hint-copy {
    font-size: 0.7rem;
  }

  .rr-graph-controls__meta-caption {
    font-size: 0.68rem;
  }

  .rr-graph-controls.is-compact .rr-graph-controls__toolbar {
    gap: 0.42rem;
    padding: 0.48rem;
  }

  .rr-graph-controls.is-compact .rr-graph-controls__search-shell {
    min-height: 38px;
    padding-inline: 0.62rem;
  }

  .rr-graph-controls.is-compact .rr-graph-controls__search-row,
  .rr-graph-controls.is-compact .rr-graph-controls__search-actions {
    gap: 0.36rem;
  }

  .rr-graph-controls.is-compact .rr-graph-controls__hint-copy {
    font-size: 0.64rem;
  }
}

@media (max-width: 920px) {
  .rr-graph-controls {
    border-radius: 14px;
  }

  .rr-graph-controls__toolbar {
    gap: 0.38rem 0.42rem;
    padding: 0.44rem;
  }

  .rr-graph-controls__search-shell {
    gap: 0.46rem;
    min-height: 36px;
    padding-inline: 0.56rem;
    border-radius: 13px;
  }

  .rr-graph-controls__result {
    grid-template-columns: minmax(0, 1fr);
    gap: 6px;
  }

  .rr-graph-controls__result-type {
    justify-self: start;
  }

  .rr-graph-controls__summary {
    gap: 0.24rem 0.3rem;
    padding-inline: 0;
    padding-bottom: 0;
  }

  .rr-graph-controls__meta {
    gap: 0.22rem;
  }

  .rr-graph-controls__badge--summary,
  .rr-graph-toolbar__artifact-toggle {
    min-height: 1.48rem;
    padding-inline: 0.44rem;
    font-size: 0.64rem;
  }

  .rr-graph-controls__meta-caption {
    font-size: 0.62rem;
    line-height: 1.24;
  }

  .rr-graph-controls__hint-copy {
    display: none;
  }
}

@media (max-width: 640px) {
  .rr-graph-controls__toolbar {
    gap: 0.22rem;
    padding: 0.26rem;
  }

  .rr-graph-controls__search-actions {
    display: flex;
    gap: 0.22rem;
    width: 100%;
  }

  .rr-graph-controls__search-shell,
  .rr-graph-controls__select-shell {
    min-height: 32px;
    border-radius: 11px;
  }

  .rr-graph-controls__select-shell {
    flex: 1 1 calc(50% - 0.11rem);
    min-width: 0;
  }

  .rr-graph-controls__field,
  .rr-graph-controls__select {
    font-size: 0.8rem;
  }

  .rr-graph-controls__actions {
    grid-column: 1 / -1;
    justify-self: end;
    min-height: 32px;
    gap: 4px;
  }

  .rr-graph-controls__icon-button {
    width: 24px;
    height: 24px;
    border-radius: 8px;
  }

  .rr-graph-controls__summary {
    gap: 0.08rem;
    padding-inline: 0.12rem;
    padding-bottom: 0.08rem;
  }

  .rr-graph-controls__badge--summary,
  .rr-graph-toolbar__artifact-toggle {
    min-height: 1.34rem;
    padding-inline: 0.34rem;
    font-size: 0.6rem;
  }

  .rr-graph-controls__meta-caption {
    display: none;
  }
}

@media (max-width: 420px) {
  .rr-graph-controls__toolbar {
    gap: 0.18rem;
    padding: 0.22rem;
  }

  .rr-graph-controls__search-shell,
  .rr-graph-controls__select-shell {
    min-height: 31px;
    padding-inline: 0.48rem;
  }

  .rr-graph-controls__field,
  .rr-graph-controls__select {
    font-size: 0.76rem;
  }

  .rr-graph-controls__search-actions {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.18rem;
  }

  .rr-graph-controls__actions {
    justify-self: end;
    gap: 3px;
  }

  .rr-graph-controls__meta {
    gap: 0.18rem;
  }

  .rr-graph-controls__summary-line {
    display: block;
  }

  .rr-graph-controls__badge--summary-dense {
    display: none;
  }
}

@media (min-width: 1800px) {
  .rr-graph-controls {
    max-width: 44rem;
  }

  .rr-graph-controls__toolbar {
    gap: 0.56rem;
    padding: 0.58rem;
  }

  .rr-graph-controls__search-actions {
    gap: 0.42rem;
  }

  .rr-graph-controls__select-shell {
    height: 38px;
    border-radius: 14px;
  }

  .rr-graph-controls__select {
    height: 36px;
    font-size: 12.5px;
  }

  .rr-graph-controls__actions {
    min-height: 38px;
    gap: 6px;
  }

  .rr-graph-controls__icon-button {
    width: 28px;
    height: 28px;
    border-radius: 10px;
  }

  .rr-graph-controls__summary {
    gap: 0.36rem;
    padding: 0 0.58rem 0.58rem;
  }

  .rr-graph-controls__hint-copy {
    padding: 0.54rem 0.62rem;
  }
}
</style>
