<script setup lang="ts">
withDefaults(
  defineProps<{
    eyebrow?: string
    title: string
    subtitle?: string
    primaryActionLabel?: string
    primaryActionDisabled?: boolean
    compact?: boolean
  }>(),
  {
    eyebrow: undefined,
    subtitle: undefined,
    primaryActionLabel: undefined,
    primaryActionDisabled: false,
    compact: false,
  },
)

const emit = defineEmits<{
  (event: 'primary-action'): void
}>()
</script>

<template>
  <header class="rr-page-header" :class="{ 'rr-page-header--compact': compact }">
    <div class="rr-page-header__copy">
      <p v-if="eyebrow" class="rr-page-header__eyebrow">
        {{ eyebrow }}
      </p>
      <h1 class="rr-page-header__title">
        {{ title }}
      </h1>
      <p v-if="subtitle" class="rr-page-header__description">
        {{ subtitle }}
      </p>
      <div v-if="$slots.meta" class="rr-page-header__meta">
        <slot name="meta" />
      </div>
    </div>

    <div v-if="$slots.actions || primaryActionLabel" class="rr-page-header__actions">
      <slot name="actions" />
      <button
        v-if="primaryActionLabel"
        type="button"
        class="rr-button rr-button--primary"
        :disabled="primaryActionDisabled"
        @click="emit('primary-action')"
      >
        {{ primaryActionLabel }}
      </button>
    </div>
  </header>
</template>
