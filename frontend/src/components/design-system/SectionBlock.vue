<script setup lang="ts">
withDefaults(
  defineProps<{
    title: string
    subtitle?: string
    compact?: boolean
  }>(),
  {
    subtitle: undefined,
    compact: false,
  },
)
</script>

<template>
  <section class="rr-section-block" :class="{ 'rr-section-block--compact': compact }">
    <header class="rr-section-block__head">
      <div class="rr-section-block__copy">
        <h2 class="rr-section-block__title">
          {{ title }}
        </h2>
        <p v-if="subtitle" class="rr-section-block__subtitle">
          {{ subtitle }}
        </p>
      </div>
      <div v-if="$slots.aside" class="rr-section-block__aside">
        <slot name="aside" />
      </div>
    </header>
    <div class="rr-section-block__body">
      <slot />
    </div>
  </section>
</template>
