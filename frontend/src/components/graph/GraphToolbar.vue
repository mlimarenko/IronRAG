<script setup lang="ts">
import type {
  GraphConvergenceStatus,
  GraphNodeType,
  GraphSearchHit,
  GraphStatus,
} from 'src/models/ui/graph'

const props = defineProps<{
  query: string
  filter: GraphNodeType | ''
  hits: GraphSearchHit[]
  nodeCount: number
  relationCount: number
  graphStatus: GraphStatus | null
  convergenceStatus: GraphConvergenceStatus | null
  rebuildBacklogCount: number
  readyNoGraphCount: number
  filteredArtifactCount: number
  focusLabel: string | null
  showFilteredArtifacts: boolean
}>()

const emit = defineEmits<{
  updateQuery: [value: string]
  updateFilter: [value: GraphNodeType | '']
  selectHit: [id: string]
  clearFocus: []
  toggleFilteredArtifacts: []
}>()
</script>

<template>
  <div class="rr-graph-toolbar">
    <div class="rr-graph-toolbar__search">
      <input
        :value="props.query"
        type="search"
        :placeholder="$t('graph.search')"
        @input="emit('updateQuery', ($event.target as HTMLInputElement).value)"
      >
      <div
        v-if="props.hits.length"
        class="rr-graph-toolbar__hits"
      >
        <button
          v-for="hit in props.hits"
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

    <select
      :value="props.filter"
      @change="emit('updateFilter', ($event.target as HTMLSelectElement).value as GraphNodeType | '')"
    >
      <option value="">{{ $t('graph.allNodeTypes') }}</option>
      <option value="document">{{ $t('graph.nodeTypes.document') }}</option>
      <option value="entity">{{ $t('graph.nodeTypes.entity') }}</option>
      <option value="topic">{{ $t('graph.nodeTypes.topic') }}</option>
    </select>

    <button
      v-if="props.filteredArtifactCount > 0 || props.showFilteredArtifacts"
      class="rr-button rr-button--ghost rr-button--tiny"
      :class="{ 'is-active': props.showFilteredArtifacts }"
      type="button"
      @click="emit('toggleFilteredArtifacts')"
    >
      {{
        props.showFilteredArtifacts
          ? $t('graph.hideFilteredArtifacts')
          : $t('graph.showFilteredArtifacts', { count: props.filteredArtifactCount })
      }}
    </button>

    <div
      v-if="props.focusLabel"
      class="rr-graph-toolbar__focus"
    >
      <span class="rr-graph-toolbar__focus-dot" />
      <span class="rr-graph-toolbar__focus-label">
        {{ $t('graph.focusLabel', { label: props.focusLabel }) }}
      </span>
      <button
        class="rr-graph-toolbar__clear"
        type="button"
        @click="emit('clearFocus')"
      >
        {{ $t('graph.clearFocus') }}
      </button>
    </div>

    <div class="rr-graph-toolbar__count">
      <span
        v-if="props.graphStatus"
        :class="`rr-graph-toolbar__status rr-graph-toolbar__status--${props.graphStatus}`"
      >
        {{ $t(`graph.statuses.${props.graphStatus}`) }}
      </span>
      <span
        v-if="props.convergenceStatus"
        class="rr-graph-toolbar__status rr-graph-toolbar__status--convergence"
      >
        {{ $t(`graph.convergence.${props.convergenceStatus}`) }}
      </span>
      {{ props.nodeCount }} {{ $t('graph.nodes') }} · {{ props.relationCount }} {{ $t('graph.relations') }}
      <small
        v-if="props.rebuildBacklogCount || props.readyNoGraphCount || props.filteredArtifactCount"
        class="rr-graph-toolbar__summary"
      >
        <span v-if="props.rebuildBacklogCount">
          {{ $t('graph.toolbarBacklog', { count: props.rebuildBacklogCount }) }}
        </span>
        <span v-if="props.readyNoGraphCount">
          {{ $t('graph.toolbarReadyNoGraph', { count: props.readyNoGraphCount }) }}
        </span>
        <span v-if="props.filteredArtifactCount">
          {{ $t('graph.toolbarFilteredArtifacts', { count: props.filteredArtifactCount }) }}
        </span>
      </small>
    </div>
  </div>
</template>
