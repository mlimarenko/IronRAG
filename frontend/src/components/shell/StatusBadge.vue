<script setup lang="ts">
import { computed } from 'vue'

import { translateStatusLabel } from 'src/i18n/helpers'

const props = withDefaults(
  defineProps<{
    status?: string | null
    label?: string | null
    tone?: 'neutral' | 'positive' | 'warning' | 'negative' | 'info'
    emphasis?: 'subtle' | 'strong'
  }>(),
  {
    status: null,
    label: null,
    tone: undefined,
    emphasis: 'subtle',
  },
)

function normalizeTone(status?: string | null) {
  const value = status?.trim().toLowerCase()

  if (!value) {
    return 'neutral'
  }

  if (
    ['healthy', 'ready', 'success', 'ok', 'active', 'completed', 'available', 'synced'].includes(
      value,
    )
  ) {
    return 'positive'
  }

  if (
    ['degraded', 'warning', 'pending', 'queued', 'validating', 'running', 'partial'].includes(value)
  ) {
    return 'warning'
  }

  if (
    ['error', 'failed', 'blocked', 'unavailable', 'misconfigured', 'offline', 'canceled'].includes(
      value,
    )
  ) {
    return 'negative'
  }

  if (['info', 'draft', 'idle', 'unknown'].includes(value)) {
    return 'info'
  }

  return 'neutral'
}

const resolvedTone = computed(() => props.tone ?? normalizeTone(props.status ?? props.label))
const resolvedLabel = computed(() => props.label ?? translateStatusLabel(props.status))
</script>

<template>
  <span
    class="status-badge"
    :data-tone="resolvedTone"
    :data-emphasis="emphasis"
  >
    {{ resolvedLabel }}
  </span>
</template>

<style scoped>
.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-height: 28px;
  padding: 0 10px;
  border-radius: var(--rr-radius-pill);
  border: 1px solid transparent;
  font-size: 0.8rem;
  font-weight: 700;
  line-height: 1;
  white-space: nowrap;
}

.status-badge[data-emphasis='subtle'] {
  background: rgb(148 163 184 / 0.12);
  color: var(--rr-color-text-secondary);
}

.status-badge[data-emphasis='strong'] {
  background: var(--rr-color-bg-contrast);
  color: var(--rr-color-text-inverse);
}

.status-badge[data-tone='positive'][data-emphasis='subtle'] {
  background: rgb(34 197 94 / 0.14);
  color: var(--rr-color-success-600);
}

.status-badge[data-tone='warning'][data-emphasis='subtle'] {
  background: rgb(245 158 11 / 0.16);
  color: var(--rr-color-warning-600);
}

.status-badge[data-tone='negative'][data-emphasis='subtle'] {
  background: rgb(239 68 68 / 0.13);
  color: var(--rr-color-danger-600);
}

.status-badge[data-tone='info'][data-emphasis='subtle'] {
  background: rgb(59 130 246 / 0.13);
  color: var(--rr-color-accent-700);
}

.status-badge[data-tone='positive'][data-emphasis='strong'] {
  background: var(--rr-color-success-600);
}

.status-badge[data-tone='warning'][data-emphasis='strong'] {
  background: var(--rr-color-warning-600);
}

.status-badge[data-tone='negative'][data-emphasis='strong'] {
  background: var(--rr-color-danger-600);
}

.status-badge[data-tone='info'][data-emphasis='strong'] {
  background: var(--rr-color-accent-700);
}
</style>
