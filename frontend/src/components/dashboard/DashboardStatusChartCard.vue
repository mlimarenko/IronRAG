<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import SurfacePanel from 'src/components/design-system/SurfacePanel.vue'
import type { DashboardChartSummary } from 'src/models/ui/dashboard'

const props = defineProps<{
  summary: DashboardChartSummary | null
}>()

const { t } = useI18n()

const segments = computed(() => {
  if (!props.summary) {
    return []
  }
  const total = Math.max(
    props.summary.segments.reduce((acc, segment) => acc + segment.value, 0),
    1,
  )
  return props.summary.segments.map((segment) => ({
    ...segment,
    width: segment.value > 0 ? Math.max((segment.value / total) * 100, 6) : 0,
  }))
})
</script>

<template>
  <SurfacePanel class="rr-dashboard-card rr-dashboard-chart">
    <header class="rr-dashboard-card__header">
      <div class="rr-dashboard-card__copy">
        <p class="rr-dashboard-card__eyebrow">{{ t('dashboard.chart.eyebrow') }}</p>
        <h2 class="rr-dashboard-card__title">{{ props.summary?.label ?? t('dashboard.chart.title') }}</h2>
        <p class="rr-dashboard-card__subtitle">{{ t('dashboard.chart.subtitle') }}</p>
      </div>
    </header>

    <div
      v-if="segments.length"
      class="rr-dashboard-chart__body"
    >
      <div class="rr-dashboard-chart__track">
        <span
          v-for="segment in segments"
          :key="segment.key"
          class="rr-dashboard-chart__segment"
          :style="{ width: `${segment.width}%`, background: segment.color ?? undefined }"
        />
      </div>
      <div class="rr-dashboard-chart__legend">
        <div
          v-for="segment in segments"
          :key="segment.key"
          class="rr-dashboard-chart__legend-item"
        >
          <span
            class="rr-dashboard-chart__swatch"
            :style="{ background: segment.color ?? undefined }"
          />
          <div>
            <strong>{{ segment.value }}</strong>
            <span>{{ segment.label }}</span>
          </div>
        </div>
      </div>
    </div>
    <p
      v-else
      class="rr-dashboard-card__empty"
    >
      {{ t('dashboard.chart.empty') }}
    </p>
  </SurfacePanel>
</template>
