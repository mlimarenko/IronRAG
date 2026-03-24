<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import SurfacePanel from 'src/components/design-system/SurfacePanel.vue'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DashboardRecentDocument } from 'src/models/ui/dashboard'

const props = defineProps<{
  documents: DashboardRecentDocument[]
}>()

const { t } = useI18n()
const router = useRouter()
const { formatDateTime } = useDisplayFormatters()

async function openDocuments() {
  await router.push('/documents')
}
</script>

<template>
  <SurfacePanel class="rr-dashboard-card rr-dashboard-recent">
    <header class="rr-dashboard-card__header">
      <div class="rr-dashboard-card__copy">
        <p class="rr-dashboard-card__eyebrow">{{ t('dashboard.recent.eyebrow') }}</p>
        <h2 class="rr-dashboard-card__title">{{ t('dashboard.recent.title') }}</h2>
        <p class="rr-dashboard-card__subtitle">{{ t('dashboard.recent.subtitle') }}</p>
      </div>
      <button
        type="button"
        class="rr-button rr-button--ghost rr-button--tiny"
        @click="openDocuments"
      >
        {{ t('shared.actions.viewAll') }}
      </button>
    </header>

    <div
      v-if="props.documents.length"
      class="rr-dashboard-recent__list"
    >
      <article
        v-for="row in props.documents"
        :key="row.id"
        class="rr-dashboard-recent__row"
      >
        <div class="rr-dashboard-recent__copy">
          <strong>{{ row.fileName }}</strong>
          <span>{{ row.fileType }} · {{ formatDateTime(row.uploadedAt) }}</span>
        </div>
        <div class="rr-dashboard-recent__actions">
          <StatusBadge
            :kind="row.status"
            :label="row.statusLabel"
          />
          <button
            type="button"
            class="rr-button rr-button--ghost rr-button--tiny"
            @click="openDocuments"
          >
            {{ t('dashboard.recent.openAction') }}
          </button>
        </div>
      </article>
    </div>
    <p
      v-else
      class="rr-dashboard-card__empty"
    >
      {{ t('dashboard.recent.empty') }}
    </p>
  </SurfacePanel>
</template>
