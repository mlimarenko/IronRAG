<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import FeedbackState from 'src/components/design-system/FeedbackState.vue'
import DashboardHero from 'src/components/dashboard/DashboardHero.vue'
import DashboardRecentDocumentsCard from 'src/components/dashboard/DashboardRecentDocumentsCard.vue'
import DashboardStatsStrip from 'src/components/dashboard/DashboardStatsStrip.vue'
import DashboardStatusChartCard from 'src/components/dashboard/DashboardStatusChartCard.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { useDashboardStore } from 'src/stores/dashboard'
import { useShellStore } from 'src/stores/shell'
import { resolveDashboardVisibleMetrics, type DashboardHeroFact } from 'src/models/ui/dashboard'

const { t } = useI18n()
const { formatDateTime } = useDisplayFormatters()
const dashboardStore = useDashboardStore()
const shellStore = useShellStore()
const { overview, error, loading, refreshIntervalMs } = storeToRefs(dashboardStore)

let refreshTimer: number | null = null
const isPageVisible = ref(typeof document === 'undefined' ? true : document.visibilityState === 'visible')

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function pollDashboard(): void {
  void dashboardStore
    .load(shellStore.context?.activeLibrary.id ?? null, { preserveUi: true })
    .catch(() => undefined)
}

function handleVisibilityChange(): void {
  isPageVisible.value = document.visibilityState === 'visible'
}

watch(
  () => shellStore.context?.activeLibrary.id ?? null,
  async (libraryId) => {
    try {
      await dashboardStore.load(libraryId)
    } catch {
      // Store error state is authoritative for page feedback.
    }
  },
  { immediate: true },
)

watch(
  [() => refreshIntervalMs.value, isPageVisible],
  ([intervalMs, pageVisible]) => {
    stopPolling()
    if (intervalMs <= 0 || !pageVisible) {
      return
    }
    refreshTimer = window.setInterval(pollDashboard, intervalMs)
  },
  { immediate: true },
)

watch(isPageVisible, (pageVisible) => {
  if (!pageVisible || refreshIntervalMs.value <= 0) {
    return
  }
  pollDashboard()
})

onMounted(() => {
  document.addEventListener('visibilitychange', handleVisibilityChange)
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', handleVisibilityChange)
  stopPolling()
})

const metrics = computed(() => overview.value?.metrics ?? [])
const visibleMetrics = computed(() => resolveDashboardVisibleMetrics(metrics.value))
const attentionItems = computed(() => overview.value?.attentionItems ?? [])
const recentDocuments = computed(() => overview.value?.recentDocuments ?? [])
const chartSummary = computed(() => overview.value?.chartSummary ?? null)
const primaryActions = computed(() => overview.value?.primaryActions ?? [])
const narrative = computed(() => overview.value?.summaryNarrative ?? t('dashboard.narrative.empty'))
const heroNarrative = computed(() => {
  const totalDocuments = Number(metrics.value.find((metric) => metric.key === 'documents')?.value ?? 0)
  const inFlightCount = Number(metrics.value.find((metric) => metric.key === 'inFlight')?.value ?? 0)

  if (totalDocuments <= 0) {
    return narrative.value
  }

  return attentionItems.value.length > 0 || inFlightCount > 0 || visibleMetrics.value.length <= 1
    ? narrative.value
    : ''
})
const isSettledOverview = computed(() => {
  const totalDocuments = Number(metrics.value.find((metric) => metric.key === 'documents')?.value ?? 0)
  const readyCount = Number(metrics.value.find((metric) => metric.key === 'ready')?.value ?? 0)
  const inFlightCount = Number(metrics.value.find((metric) => metric.key === 'inFlight')?.value ?? 0)

  return totalDocuments > 0 && readyCount >= totalDocuments && inFlightCount === 0 && attentionItems.value.length === 0
})
const showStatusChart = computed(() => Boolean(chartSummary.value) && !isSettledOverview.value)
const showStatsStrip = computed(() => visibleMetrics.value.length > 1)
const compactRecentDocuments = computed(
  () => !showStatusChart.value && recentDocuments.value.length > 0 && recentDocuments.value.length <= 3,
)
const heroFacts = computed<DashboardHeroFact[]>(() => {
  const shellContext = shellStore.context
  const facts: DashboardHeroFact[] = []
  const latestDocument = recentDocuments.value[0] ?? null
  const firstAttention = attentionItems.value[0] ?? null
  const totalDocuments = Number(metrics.value.find((metric) => metric.key === 'documents')?.value ?? 0)

  if (shellContext) {
    facts.push({
      key: 'library',
      label: t('dashboard.heroFacts.library'),
      value: shellContext.activeLibrary.name,
      supportingText: t('dashboard.heroFacts.libraryHint'),
      tone: 'accent',
    })
  }

  facts.push({
    key: 'latestUpload',
    label: t('dashboard.heroFacts.latestUpload'),
    value: latestDocument
      ? formatDateTime(latestDocument.uploadedAt)
      : t('dashboard.heroFacts.noUploadsValue'),
    supportingText: latestDocument?.fileName ?? t('dashboard.heroFacts.noUploadsHint'),
    tone: latestDocument ? 'default' : 'warning',
  })

  if (isSettledOverview.value && totalDocuments > 0) {
    facts.push({
      key: 'documents',
      label: t('dashboard.heroFacts.documents'),
      value: String(totalDocuments),
      supportingText: t('dashboard.heroFacts.documentsHint'),
      tone: 'success',
    })
  }

  if (!isSettledOverview.value && attentionItems.value.length === 0) {
    facts.push({
      key: 'nextCheck',
      label: t('dashboard.heroFacts.nextCheck'),
      value: t('dashboard.heroFacts.quietValue'),
      supportingText: firstAttention?.title ?? t('dashboard.heroFacts.quietHint'),
      tone: 'success',
    })
  }

  return facts
})
</script>

<template>
  <div class="rr-dashboard">
    <FeedbackState
      v-if="error && !overview"
      :title="t('shared.feedbackState.error')"
      :message="error"
      kind="error"
    />
    <FeedbackState
      v-else-if="loading && !overview"
      :title="t('shared.feedbackState.loading')"
      :message="t('dashboard.loadingDescription')"
      kind="loading"
    />
    <div
      v-else
      class="rr-dashboard__layout"
    >
      <div v-if="error && overview" class="rr-stale-banner" role="alert">
        {{ t('dashboard.staleData', 'Данные могут быть устаревшими') }}
      </div>

      <div
        class="rr-dashboard__overview"
        :class="{ 'is-solo': !showStatsStrip }"
      >
        <DashboardHero
          :narrative="heroNarrative"
          :actions="primaryActions"
          :facts="heroFacts"
          :refresh-loading="loading"
          :attention-items="attentionItems"
          :compact="!showStatsStrip"
          @refresh="dashboardStore.load(shellStore.context?.activeLibrary.id ?? null)"
        />

        <DashboardStatsStrip
          v-if="showStatsStrip"
          :metrics="metrics"
        />
      </div>

      <div
        class="rr-dashboard__workbench"
        :class="{ 'is-settled': !showStatusChart }"
      >
        <DashboardStatusChartCard
          v-if="showStatusChart"
          :summary="chartSummary"
        />
        <DashboardRecentDocumentsCard
          :documents="recentDocuments"
          :compact="compactRecentDocuments"
        />
      </div>
    </div>
  </div>
</template>
