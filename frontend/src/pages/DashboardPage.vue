<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import FeedbackState from 'src/components/design-system/FeedbackState.vue'
import PageFrame from 'src/components/design-system/PageFrame.vue'
import DashboardAttentionCard from 'src/components/dashboard/DashboardAttentionCard.vue'
import DashboardHero from 'src/components/dashboard/DashboardHero.vue'
import DashboardRecentDocumentsCard from 'src/components/dashboard/DashboardRecentDocumentsCard.vue'
import DashboardStatsStrip from 'src/components/dashboard/DashboardStatsStrip.vue'
import DashboardStatusChartCard from 'src/components/dashboard/DashboardStatusChartCard.vue'
import { useDashboardStore } from 'src/stores/dashboard'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const dashboardStore = useDashboardStore()
const shellStore = useShellStore()
const { overview, error, loading, refreshIntervalMs } = storeToRefs(dashboardStore)

let refreshTimer: number | null = null

function stopPolling() {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
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
  () => refreshIntervalMs.value,
  (intervalMs) => {
    stopPolling()
    if (intervalMs <= 0) {
      return
    }
    refreshTimer = window.setInterval(() => {
      void dashboardStore
        .load(shellStore.context?.activeLibrary.id ?? null, { preserveUi: true })
        .catch(() => undefined)
    }, intervalMs)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopPolling()
})

const metrics = computed(() => overview.value?.metrics ?? [])
const attentionItems = computed(() => overview.value?.attentionItems ?? [])
const recentDocuments = computed(() => overview.value?.recentDocuments ?? [])
const chartSummary = computed(() => overview.value?.chartSummary ?? null)
const primaryActions = computed(() => overview.value?.primaryActions ?? [])
const narrative = computed(() => overview.value?.summaryNarrative ?? t('dashboard.narrative.empty'))
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
    <PageFrame
      v-else
      width-mode="wide"
    >
      <template #header>
        <DashboardHero
          :narrative="narrative"
          :actions="primaryActions"
        />
      </template>
      <template #primary>
        <div class="rr-dashboard__primary">
          <DashboardStatsStrip :metrics="metrics" />
          <DashboardStatusChartCard :summary="chartSummary" />
          <DashboardRecentDocumentsCard :documents="recentDocuments" />
        </div>
      </template>
      <template #secondary>
        <DashboardAttentionCard :items="attentionItems" />
      </template>
    </PageFrame>
  </div>
</template>
