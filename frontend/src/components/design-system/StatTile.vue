<script setup lang="ts">
import SurfacePanel from './SurfacePanel.vue'

withDefaults(defineProps<{
  label: string
  value: string | number
  supportingText?: string
  statusKind?: 'ready' | 'warning' | 'failed' | 'info'
}>(), {
  supportingText: undefined,
  statusKind: 'info',
})
</script>

<template>
  <SurfacePanel class="rr-stat-tile">
    <p class="rr-stat-tile__label">
      {{ label }}
    </p>
    <p class="rr-stat-tile__value">
      {{ value }}
    </p>
    <p
      v-if="supportingText"
      class="rr-stat-tile__support"
      :class="`is-${statusKind}`"
    >
      {{ supportingText }}
    </p>
  </SurfacePanel>
</template>
