<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { DashboardChartSummary } from 'src/models/ui/dashboard'

const props = defineProps<{
  summary: DashboardChartSummary | null
  compact?: boolean
}>()

const { t } = useI18n()

const totalCount = computed(
  () => props.summary?.segments.reduce((acc, segment) => acc + segment.value, 0) ?? 0,
)

const nonZeroSegments = computed(
  () => props.summary?.segments.filter((segment) => segment.value > 0) ?? [],
)

const barSegments = computed(() => {
  if (!props.summary) return []
  const total = Math.max(totalCount.value, 1)
  return props.summary.segments
    .filter((s) => s.value > 0)
    .map((s) => ({
      ...s,
      pct: Math.max((s.value / total) * 100, 4),
    }))
})

const hasData = computed(() => totalCount.value > 0)
const graphReadySegment = computed(
  () =>
    props.summary?.segments.find(
      (segment) => segment.key === 'graphReady' || segment.key === 'ready',
    ) ?? null,
)
const graphSparseSegment = computed(
  () => props.summary?.segments.find((segment) => segment.key === 'graphSparse') ?? null,
)
const processingSegment = computed(
  () => props.summary?.segments.find((segment) => segment.key === 'processing') ?? null,
)
const failedSegment = computed(
  () => props.summary?.segments.find((segment) => segment.key === 'failed') ?? null,
)
const singleStatus = computed(() =>
  nonZeroSegments.value.length === 1 ? nonZeroSegments.value[0] : null,
)
const mixedStatus = computed(() => nonZeroSegments.value.length > 1)
const settledReady = computed(
  () => singleStatus.value?.key === 'ready' || singleStatus.value?.key === 'graphReady',
)
const leanMode = computed(() => {
  if (props.compact) {
    return true
  }

  if (!props.summary || settledReady.value) {
    return false
  }

  return (processingSegment.value?.value ?? 0) === 0 && nonZeroSegments.value.length <= 2
})
const summaryLine = computed(() => {
  if (!props.summary) {
    return ''
  }

  if (settledReady.value) {
    return ''
  }

  if (singleStatus.value) {
    return t('dashboard.chart.summarySingleStatus', {
      count: singleStatus.value.value,
      status: singleStatus.value.label.toLowerCase(),
    })
  }

  return t('dashboard.chart.summaryMixed', {
    graphReady: graphReadySegment.value?.value ?? 0,
    graphSparse: graphSparseSegment.value?.value ?? 0,
    processing: processingSegment.value?.value ?? 0,
    failed: failedSegment.value?.value ?? 0,
  })
})
</script>

<template>
  <section
    v-if="hasData"
    class="rr-dash-chart"
    :class="{ 'is-settled': settledReady, 'is-lean': leanMode }"
  >
    <header class="rr-dash-chart__head">
      <div class="rr-dash-chart__copy">
        <p class="rr-dash-chart__eyebrow">{{ t('dashboard.chart.eyebrow') }}</p>
        <h2 class="rr-dash-chart__title">
          {{ props.summary?.label ?? t('dashboard.chart.title') }}
        </h2>
        <p class="rr-dash-chart__subtitle">{{ t('dashboard.chart.subtitle') }}</p>
      </div>
    </header>

    <div class="rr-dash-chart__bar">
      <span
        v-for="seg in barSegments"
        :key="seg.key"
        class="rr-dash-chart__seg"
        :style="{ flex: `${seg.pct} 0 0%`, background: seg.color ?? undefined }"
        :title="`${seg.label}: ${seg.value}`"
      />
    </div>

    <p v-if="summaryLine" class="rr-dash-chart__summary">
      {{ summaryLine }}
    </p>

    <ul v-if="mixedStatus" class="rr-dash-chart__legend">
      <li v-for="seg in nonZeroSegments" :key="seg.key" class="rr-dash-chart__legend-item">
        <span class="rr-dash-chart__dot" :style="{ background: seg.color ?? undefined }" />
        <strong>{{ seg.value }}</strong>
        <span>{{ seg.label }}</span>
      </li>
    </ul>
  </section>
</template>
