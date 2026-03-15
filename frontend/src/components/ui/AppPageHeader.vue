<script setup lang="ts">
import StatusBadge from 'src/components/shell/StatusBadge.vue'

withDefaults(
  defineProps<{
    eyebrow?: string
    title: string
    description?: string
    status?: string
    statusLabel?: string
    titleTag?: 'h1' | 'h2' | 'h3'
    compact?: boolean
    hideActions?: boolean
  }>(),
  {
    eyebrow: undefined,
    description: undefined,
    status: undefined,
    statusLabel: undefined,
    titleTag: 'h1',
    compact: false,
    hideActions: false,
  },
)
</script>

<template>
  <header class="rr-page-header" :class="{ 'rr-page-header--compact': compact }">
    <div class="rr-page-header__copy">
      <p v-if="eyebrow" class="rr-page-header__eyebrow rr-kicker">
        {{ eyebrow }}
      </p>

      <div class="rr-page-header__title-row">
        <component :is="titleTag" class="rr-page-header__title">
          {{ title }}
        </component>

        <StatusBadge
          v-if="status || statusLabel"
          :status="status"
          :label="statusLabel"
          emphasis="strong"
        />
      </div>

      <p v-if="description" class="rr-page-header__description">
        {{ description }}
      </p>
    </div>

    <div v-if="$slots.actions && !hideActions" class="rr-page-header__actions">
      <slot name="actions" />
    </div>
  </header>
</template>
