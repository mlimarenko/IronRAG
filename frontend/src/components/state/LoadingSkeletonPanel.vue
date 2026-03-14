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
  <article
    class="loading-skeleton-panel"
    aria-busy="true"
    aria-live="polite"
  >
    <div class="loading-skeleton-panel__header">
      <span class="loading-skeleton-panel__badge">{{ title }}</span>
      <div class="loading-skeleton-panel__line loading-skeleton-panel__line--short" />
    </div>
    <div class="loading-skeleton-panel__body">
      <div
        v-for="index in lines"
        :key="index"
        class="loading-skeleton-panel__line"
        :class="{ 'loading-skeleton-panel__line--short': index === lines }"
      />
    </div>
  </article>
</template>

<style scoped>
.loading-skeleton-panel {
  display: grid;
  gap: 16px;
  padding: 20px;
  border: 1px solid #d7dee7;
  border-radius: 18px;
  background: rgb(255 255 255 / 0.7);
}

.loading-skeleton-panel__header,
.loading-skeleton-panel__body {
  display: grid;
  gap: 12px;
}

.loading-skeleton-panel__badge,
.loading-skeleton-panel__line {
  position: relative;
  overflow: hidden;
  border-radius: 999px;
  background: #e2e8f0;
}

.loading-skeleton-panel__badge {
  width: 96px;
  height: 22px;
  color: transparent;
}

.loading-skeleton-panel__line {
  height: 14px;
}

.loading-skeleton-panel__line--short {
  width: 60%;
}

.loading-skeleton-panel__badge::after,
.loading-skeleton-panel__line::after {
  content: '';
  position: absolute;
  inset: 0;
  transform: translateX(-100%);
  background: linear-gradient(90deg, transparent, rgb(255 255 255 / 0.7), transparent);
  animation: skeleton-wave 1.3s ease-in-out infinite;
}

@keyframes skeleton-wave {
  100% {
    transform: translateX(100%);
  }
}
</style>
