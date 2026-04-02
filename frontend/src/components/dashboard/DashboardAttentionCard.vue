<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import SurfacePanel from 'src/components/design-system/SurfacePanel.vue'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import type { DashboardAttentionItem } from 'src/models/ui/dashboard'

const props = defineProps<{
  items: DashboardAttentionItem[]
}>()

const { t } = useI18n()
const router = useRouter()

function badgeKind(severity: DashboardAttentionItem['severity']): 'info' | 'warning' | 'failed' {
  if (severity === 'warning') {
    return 'warning'
  }
  if (severity === 'error') {
    return 'failed'
  }
  return 'info'
}

function severityLabel(severity: DashboardAttentionItem['severity']): string {
  return t(`dashboard.attentionSeverity.${severity}`)
}

async function handleAction(item: DashboardAttentionItem) {
  if (!item.targetRoute) {
    return
  }
  await router.push(item.targetRoute)
}
</script>

<template>
  <SurfacePanel class="rr-dashboard-card rr-dashboard-attention">
    <header class="rr-dashboard-card__header">
      <div class="rr-dashboard-card__copy">
        <p class="rr-dashboard-card__eyebrow">{{ t('dashboard.attention.eyebrow') }}</p>
        <h2 class="rr-dashboard-card__title">{{ t('dashboard.attention.title') }}</h2>
        <p class="rr-dashboard-card__subtitle">{{ t('dashboard.attention.subtitle') }}</p>
      </div>
    </header>

    <ul v-if="props.items.length" class="rr-dashboard-attention__list">
      <li v-for="item in props.items" :key="item.id" class="rr-dashboard-attention__item">
        <div class="rr-dashboard-attention__copy">
          <div class="rr-dashboard-attention__meta">
            <StatusBadge :kind="badgeKind(item.severity)" :label="severityLabel(item.severity)" />
            <strong>{{ item.title }}</strong>
          </div>
          <p>{{ item.message }}</p>
        </div>
        <button
          v-if="item.actionLabel && item.targetRoute"
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="handleAction(item)"
        >
          {{ item.actionLabel }}
        </button>
      </li>
    </ul>
    <p v-else class="rr-dashboard-card__empty">
      {{ t('dashboard.attention.empty') }}
    </p>
  </SurfacePanel>
</template>
