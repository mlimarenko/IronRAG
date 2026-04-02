<script setup lang="ts">
import { computed } from 'vue'
import { resolveDashboardVisibleMetrics, type DashboardMetric } from 'src/models/ui/dashboard'

const props = defineProps<{
  metrics: DashboardMetric[]
}>()

const tiles = computed(() => resolveDashboardVisibleMetrics(props.metrics))

const columnCount = computed(() => Math.max(1, Math.min(tiles.value.length, 4)))

function toneClass(metric: DashboardMetric): string {
  const val = Number(metric.value)
  if (metric.key === 'attention') return val > 0 ? 'is-warning' : 'is-quiet'
  if (metric.key === 'inFlight') return val > 0 ? 'is-active' : 'is-quiet'
  if (metric.key === 'ready' || metric.key === 'graphReady')
    return val > 0 ? 'is-ready' : 'is-quiet'
  return ''
}
</script>

<template>
  <div class="rr-dash-stats" :style="{ '--rr-dash-stats-columns': `${columnCount}` }">
    <div v-for="metric in tiles" :key="metric.key" class="rr-dash-stat" :class="toneClass(metric)">
      <span class="rr-dash-stat__value">{{ metric.value }}</span>
      <span class="rr-dash-stat__label">{{ metric.label }}</span>
      <span v-if="metric.supportingText" class="rr-dash-stat__hint">
        {{ metric.supportingText }}
      </span>
    </div>
  </div>
</template>
