<script setup lang="ts">
import { computed } from 'vue'
import StatTile from 'src/components/design-system/StatTile.vue'
import type { DashboardMetric } from 'src/models/ui/dashboard'

const props = defineProps<{
  metrics: DashboardMetric[]
}>()

const tiles = computed(() => props.metrics.slice(0, 4))

function metricCount(metric: DashboardMetric): number {
  const value = Number(metric.value)
  return Number.isNaN(value) ? 0 : value
}

function metricTone(metric: DashboardMetric): 'info' | 'warning' | 'ready' | 'failed' {
  const count = metricCount(metric)
  if (metric.key === 'attention') {
    return count > 0 ? 'warning' : 'ready'
  }
  if (metric.key === 'inFlight') {
    return count > 0 ? 'warning' : 'info'
  }
  return 'info'
}
</script>

<template>
  <div class="rr-dashboard-metrics">
    <StatTile
      v-for="metric in tiles"
      :key="metric.key"
      :label="metric.label"
      :value="metric.value"
      :supporting-text="metric.supportingText ?? undefined"
      :status-kind="metricTone(metric)"
    />
  </div>
</template>
