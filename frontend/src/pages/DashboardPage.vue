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
const isPageVisible = ref(
  typeof document === 'undefined' ? true : document.visibilityState === 'visible',
)

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
const documentCounts = computed(() => overview.value?.documentCounts ?? null)
const narrative = computed(() => overview.value?.summaryNarrative ?? t('dashboard.narrative.empty'))
const heroNarrative = computed(() => {
  const totalDocuments = documentCounts.value?.totalDocuments ?? 0
  const inFlightCount = documentCounts.value?.inFlightDocuments ?? 0
  const latestDocument = recentDocuments.value[0] ?? null

  if (totalDocuments <= 0) {
    return narrative.value
  }

  if (attentionItems.value.length > 0) {
    return ''
  }

  if (isSettledOverview.value && totalDocuments <= 1 && attentionItems.value.length === 0) {
    return t('dashboard.narrativeCalm.single')
  }

  if (inFlightCount > 0) {
    if (latestDocument) {
      return t('dashboard.narrativeCalm.processingWithLatest', {
        count: totalDocuments,
        active: inFlightCount,
        latest: formatDateTime(latestDocument.uploadedAt),
      })
    }
    return t('dashboard.narrativeCalm.processing', {
      count: totalDocuments,
      active: inFlightCount,
    })
  }

  if (attentionItems.value.length === 0 && inFlightCount === 0) {
    if (latestDocument) {
      return t('dashboard.narrativeCalm.withLatest', {
        count: totalDocuments,
        latest: formatDateTime(latestDocument.uploadedAt),
      })
    }
    return t('dashboard.narrativeCalm.totalOnly', { count: totalDocuments })
  }

  return attentionItems.value.length > 0 || visibleMetrics.value.length <= 1 ? narrative.value : ''
})
const isSettledOverview = computed(() => {
  const totalDocuments = documentCounts.value?.totalDocuments ?? 0
  const readyCount = documentCounts.value?.graphReadyDocuments ?? 0
  const inFlightCount = documentCounts.value?.inFlightDocuments ?? 0

  return (
    totalDocuments > 0 &&
    readyCount >= totalDocuments &&
    inFlightCount === 0 &&
    attentionItems.value.length === 0
  )
})
const nonZeroChartSegments = computed(
  () => chartSummary.value?.segments.filter((segment) => segment.value > 0) ?? [],
)
const showStatusChart = computed(() => {
  if (!chartSummary.value || isSettledOverview.value) {
    return false
  }

  if (attentionItems.value.length > 0) {
    return true
  }

  const activeOrFailed =
    (documentCounts.value?.inFlightDocuments ?? 0) > 0 ||
    (documentCounts.value?.failedDocuments ?? 0) > 0
  if (activeOrFailed) {
    return true
  }

  return nonZeroChartSegments.value.length > 1
})
const compactStatusChart = computed(
  () => attentionItems.value.length > 0 || recentDocuments.value.length <= 3,
)
const showStatsStrip = computed(() => visibleMetrics.value.length > 1)
const compactRecentDocuments = computed(() => !showStatusChart.value)
const showSingleSettledCard = computed(
  () => !showStatusChart.value && recentDocuments.value.length === 1,
)
const useCalmLayout = computed(() => !showEmptyOverview.value && !showStatusChart.value)
const showEmptyOverview = computed(() => (documentCounts.value?.totalDocuments ?? 0) === 0)
const heroFacts = computed<DashboardHeroFact[]>(() => {
  const shellContext = shellStore.context
  const facts: DashboardHeroFact[] = []
  const latestDocument = recentDocuments.value[0] ?? null
  const totalDocuments = documentCounts.value?.totalDocuments ?? 0
  const readableDocuments = documentCounts.value?.readableDocuments ?? 0
  const graphSparseDocuments = documentCounts.value?.graphSparseDocuments ?? 0
  const graphReadyDocuments = documentCounts.value?.graphReadyDocuments ?? 0
  const inFlightDocuments = documentCounts.value?.inFlightDocuments ?? 0

  if (totalDocuments <= 0) {
    return facts
  }

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
    key: 'documents',
    label: t('dashboard.heroFacts.documents'),
    value: String(totalDocuments),
    supportingText: t('dashboard.heroFacts.documentsGraphSparseHint', {
      readable: readableDocuments,
      graphSparse: graphSparseDocuments,
      graphReady: graphReadyDocuments,
    }),
    tone:
      graphReadyDocuments >= totalDocuments && graphSparseDocuments === 0 && inFlightDocuments === 0
        ? 'success'
        : 'default',
  })

  if (
    latestDocument &&
    (!isSettledOverview.value || attentionItems.value.length > 0 || inFlightDocuments > 0)
  ) {
    facts.push({
      key: 'latestUpload',
      label: t('dashboard.heroFacts.latestUpload'),
      value: formatDateTime(latestDocument.uploadedAt),
      supportingText: latestDocument.fileName,
      tone: 'default',
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
      :class="{ 'is-empty': showEmptyOverview, 'is-calm': useCalmLayout }"
    >
      <div v-if="error && overview" class="rr-stale-banner" role="alert">
        {{ t('dashboard.staleData', 'Данные могут быть устаревшими') }}
      </div>

      <DashboardHero
        v-if="showEmptyOverview"
        class="rr-dashboard__hero-standalone"
        :narrative="heroNarrative"
        :actions="primaryActions"
        :facts="heroFacts"
        :refresh-loading="loading"
        :attention-items="attentionItems"
        :compact="false"
        @refresh="dashboardStore.load(shellStore.context?.activeLibrary.id ?? null)"
      />

      <template v-else>
        <div
          class="rr-dashboard__overview"
          :class="{
            'is-solo': !showStatsStrip,
            'is-solo-doc': showSingleSettledCard,
            'is-calm': useCalmLayout,
          }"
        >
          <DashboardHero
            :narrative="heroNarrative"
            :actions="primaryActions"
            :facts="heroFacts"
            :refresh-loading="loading"
            :attention-items="attentionItems"
            :compact="true"
            @refresh="dashboardStore.load(shellStore.context?.activeLibrary.id ?? null)"
          />

          <DashboardStatsStrip v-if="showStatsStrip" :metrics="metrics" />
        </div>

        <div
          class="rr-dashboard__workbench"
          :class="{
            'is-settled': !showStatusChart,
            'is-solo-doc': showSingleSettledCard,
            'is-calm': useCalmLayout,
          }"
        >
          <DashboardStatusChartCard
            v-if="showStatusChart"
            :summary="chartSummary"
            :compact="compactStatusChart"
          />
          <DashboardRecentDocumentsCard
            :documents="recentDocuments"
            :compact="compactRecentDocuments"
          />
        </div>
      </template>
    </div>
  </div>
</template>
