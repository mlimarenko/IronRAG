<script setup lang="ts">
import { computed } from 'vue'

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

function formatLabel(value?: string | null) {
  if (!value) {
    return 'Unknown'
  }

  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (char) => char.toUpperCase())
}

const resolvedTone = computed(() => props.tone ?? normalizeTone(props.status ?? props.label))
const resolvedLabel = computed(() => props.label ?? formatLabel(props.status))
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
  border-radius: 999px;
  border: 1px solid transparent;
  font-size: 0.8rem;
  font-weight: 700;
  line-height: 1;
  white-space: nowrap;
}

.status-badge[data-emphasis='subtle'] {
  background: rgb(148 163 184 / 0.12);
  color: #475569;
}

.status-badge[data-emphasis='strong'] {
  background: #0f172a;
  color: #f8fafc;
}

.status-badge[data-tone='positive'][data-emphasis='subtle'] {
  background: rgb(34 197 94 / 0.14);
  color: #166534;
}

.status-badge[data-tone='warning'][data-emphasis='subtle'] {
  background: rgb(245 158 11 / 0.16);
  color: #92400e;
}

.status-badge[data-tone='negative'][data-emphasis='subtle'] {
  background: rgb(239 68 68 / 0.13);
  color: #b91c1c;
}

.status-badge[data-tone='info'][data-emphasis='subtle'] {
  background: rgb(59 130 246 / 0.13);
  color: #1d4ed8;
}

.status-badge[data-tone='positive'][data-emphasis='strong'] {
  background: #166534;
}

.status-badge[data-tone='warning'][data-emphasis='strong'] {
  background: #92400e;
}

.status-badge[data-tone='negative'][data-emphasis='strong'] {
  background: #991b1b;
}

.status-badge[data-tone='info'][data-emphasis='strong'] {
  background: #1d4ed8;
}
</style>
