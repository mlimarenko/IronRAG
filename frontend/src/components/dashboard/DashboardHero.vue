<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import type {
  DashboardAttentionItem,
  DashboardHeroFact,
  DashboardPrimaryAction,
} from 'src/models/ui/dashboard'

const props = defineProps<{
  narrative: string
  actions: DashboardPrimaryAction[]
  facts?: DashboardHeroFact[]
  refreshLoading?: boolean
  attentionItems?: DashboardAttentionItem[]
  compact?: boolean
}>()

const emit = defineEmits<{
  refresh: []
}>()

const { t } = useI18n()
const router = useRouter()

async function handleAction(action: DashboardPrimaryAction) {
  if (!action.route) return
  await router.push(action.route)
}

async function handleAttention(item: DashboardAttentionItem) {
  if (!item.targetRoute) return
  await router.push(item.targetRoute)
}

function severityIcon(severity: DashboardAttentionItem['severity']): string {
  if (severity === 'error') return '✕'
  if (severity === 'warning') return '!'
  return 'i'
}
</script>

<template>
  <header class="rr-dash-hero" :class="{ 'is-compact': compact }">
    <div class="rr-dash-hero__top">
      <div class="rr-dash-hero__copy">
        <h1 class="rr-dash-hero__title">{{ t('dashboard.title') }}</h1>
        <p v-if="props.narrative.trim().length" class="rr-dash-hero__subtitle">
          {{ props.narrative }}
        </p>
      </div>
      <div class="rr-dash-hero__actions">
        <button
          v-for="(action, index) in props.actions"
          :key="action.key"
          type="button"
          class="rr-button"
          :class="index === 0 ? 'rr-button--primary' : 'rr-button--ghost'"
          @click="handleAction(action)"
        >
          {{ action.label }}
        </button>
        <button
          class="rr-button rr-button--ghost rr-button--compact"
          :disabled="props.refreshLoading"
          @click="emit('refresh')"
        >
          ↻ {{ t('dashboard.refresh', 'Обновить') }}
        </button>
      </div>
    </div>

    <div
      v-if="props.facts?.length"
      class="rr-dash-hero__facts"
      :class="{ 'rr-dash-hero__facts--inline': compact }"
      :style="{ '--rr-dash-hero-fact-columns': `${Math.min(props.facts.length, 4)}` }"
    >
      <article
        v-for="fact in props.facts"
        :key="fact.key"
        class="rr-dash-hero__fact"
        :class="`rr-dash-hero__fact--${fact.tone}`"
      >
        <template v-if="compact">
          <span class="rr-dash-hero__fact-inline-label">{{ fact.label }}</span>
          <strong class="rr-dash-hero__fact-inline-value">{{ fact.value }}</strong>
        </template>
        <template v-else>
          <p class="rr-dash-hero__fact-label">{{ fact.label }}</p>
          <strong class="rr-dash-hero__fact-value">{{ fact.value }}</strong>
          <p v-if="fact.supportingText" class="rr-dash-hero__fact-meta">
            {{ fact.supportingText }}
          </p>
        </template>
      </article>
    </div>

    <div v-if="props.attentionItems?.length" class="rr-dash-hero__alerts">
      <button
        v-for="item in props.attentionItems"
        :key="item.id"
        type="button"
        class="rr-dash-alert"
        :class="`rr-dash-alert--${item.severity}`"
        @click="handleAttention(item)"
      >
        <span class="rr-dash-alert__icon">{{ severityIcon(item.severity) }}</span>
        <span class="rr-dash-alert__text">
          <strong>{{ item.title }}</strong> — {{ item.message }}
        </span>
        <span v-if="item.actionLabel" class="rr-dash-alert__action">
          {{ item.actionLabel }} →
        </span>
      </button>
    </div>
  </header>
</template>
