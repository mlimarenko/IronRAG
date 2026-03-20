<script setup lang="ts">
import type { GraphConvergenceStatus, GraphLegendItem } from 'src/models/ui/graph'

defineProps<{
  items: GraphLegendItem[]
  convergenceStatus: GraphConvergenceStatus | null
  filteredArtifactCount: number
  activeProvenanceOnly: boolean
  showFilteredArtifacts: boolean
}>()
</script>

<template>
  <div class="rr-graph-legend">
    <strong>{{ $t('graph.legend') }}</strong>
    <div class="rr-graph-legend__items">
      <span
        v-for="item in items"
        :key="item.key"
        :class="`rr-graph-legend__item rr-graph-legend__item--${item.key}`"
      >
        {{ $t(`graph.legendKinds.${item.key}`) }}
      </span>
    </div>
    <p
      v-if="activeProvenanceOnly"
      class="rr-graph-legend__note"
    >
      {{ $t('graph.admittedOnlyHint') }}
    </p>
    <p
      v-if="convergenceStatus && convergenceStatus !== 'current'"
      class="rr-graph-legend__note"
    >
      {{ $t(`graph.convergenceDescriptions.${convergenceStatus}`) }}
    </p>
    <p
      v-if="filteredArtifactCount > 0"
      class="rr-graph-legend__note"
    >
      {{
        showFilteredArtifacts
          ? $t('graph.showingFilteredArtifactsHint')
          : $t('graph.filteredArtifactsHint', { count: filteredArtifactCount })
      }}
    </p>
    <p class="rr-graph-legend__note">
      {{ $t('graph.summary.legend') }}
    </p>
  </div>
</template>
