<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { computed } from 'vue'
import type { DashboardRecentDocument } from 'src/models/ui/dashboard'

const props = defineProps<{
  documents: DashboardRecentDocument[]
  compact?: boolean
}>()

const { t } = useI18n()
const router = useRouter()
const { formatCompactDateTime } = useDisplayFormatters()
const showFileType = computed(() => new Set(props.documents.map((row) => row.fileType)).size > 1)
const showStatusBadge = computed(() => props.documents.some((row) => !['ready', 'ready_no_graph'].includes(row.status)))
function rowMeta(row: DashboardRecentDocument): string {
  return `${row.fileSizeLabel} · ${formatCompactDateTime(row.uploadedAt)}`
}

async function openDocuments() {
  await router.push('/documents')
}
</script>

<template>
  <section
    class="rr-dash-docs"
    :class="{ 'is-compact': props.compact, 'is-solo': props.documents.length <= 1 }"
  >
    <header class="rr-dash-docs__header">
      <div class="rr-dash-docs__copy">
        <div class="rr-dash-docs__title-row">
          <h2 class="rr-dash-docs__title">{{ t('dashboard.recent.title') }}</h2>
        </div>
        <p class="rr-dash-docs__subtitle">{{ t('dashboard.recent.subtitle') }}</p>
      </div>
      <button
        type="button"
        class="rr-button rr-button--ghost rr-button--tiny rr-dash-docs__view-all"
        @click="openDocuments"
      >
        {{ t('shared.actions.viewAll') }}
      </button>
    </header>

    <div
      v-if="props.documents.length"
      class="rr-dash-docs__table"
    >
      <div
        v-for="row in props.documents"
        :key="row.id"
        class="rr-dash-docs__row"
      >
        <div class="rr-dash-docs__name">
          <strong>{{ row.fileName }}</strong>
          <span class="rr-dash-docs__meta rr-dash-docs__meta--inline">{{ rowMeta(row) }}</span>
        </div>
        <span
          v-if="showFileType"
          class="rr-dash-docs__meta rr-dash-docs__meta--type"
        >
          {{ row.fileType }}
        </span>
        <StatusBadge
          v-if="showStatusBadge"
          class="rr-dash-docs__status"
          :kind="row.status"
          :label="row.statusLabel"
        />
      </div>
    </div>
    <p
      v-else
      class="rr-dash-docs__empty"
    >
      {{ t('dashboard.recent.empty') }}
    </p>
  </section>
</template>
