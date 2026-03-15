<script setup lang="ts">
withDefaults(
  defineProps<{
    lines?: number
    title?: string
  }>(),
  {
    lines: 4,
    title: 'Loading',
  },
)
</script>

<template>
  <article class="rr-panel rr-skeleton-panel" aria-busy="true" aria-live="polite">
    <div class="rr-skeleton-panel__header">
      <span class="rr-skeleton-panel__badge">{{ title }}</span>
      <div class="rr-skeleton-panel__line rr-skeleton-panel__line--short" />
    </div>
    <div class="rr-skeleton-panel__body">
      <div
        v-for="index in lines"
        :key="index"
        class="rr-skeleton-panel__line"
        :class="{ 'rr-skeleton-panel__line--short': index === lines }"
      />
    </div>
  </article>
</template>
