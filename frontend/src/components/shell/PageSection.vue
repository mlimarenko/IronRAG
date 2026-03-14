<script setup lang="ts">
import StatusBadge from './StatusBadge.vue'

withDefaults(
  defineProps<{
    eyebrow?: string
    title: string
    description?: string
    status?: string
    statusLabel?: string
  }>(),
  {
    eyebrow: undefined,
    description: undefined,
    status: undefined,
    statusLabel: undefined,
  },
)
</script>

<template>
  <section class="page-section">
    <header class="page-section__header rr-panel">
      <div class="page-section__copy">
        <p
          v-if="eyebrow"
          class="page-section__eyebrow rr-kicker"
        >
          {{ eyebrow }}
        </p>
        <div class="page-section__title-row">
          <h1>{{ title }}</h1>
          <StatusBadge
            v-if="status || statusLabel"
            :status="status"
            :label="statusLabel"
            emphasis="strong"
          />
        </div>
        <p
          v-if="description"
          class="page-section__description"
        >
          {{ description }}
        </p>
      </div>
      <div
        v-if="$slots.actions"
        class="page-section__actions"
      >
        <slot name="actions" />
      </div>
    </header>

    <div
      v-if="$slots.default"
      class="page-section__body rr-page-grid"
    >
      <slot />
    </div>
  </section>
</template>

<style scoped>
.page-section {
  display: grid;
  gap: var(--rr-space-6);
}

.page-section__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-5);
  align-items: flex-start;
  padding: var(--rr-space-7);
  border-radius: var(--rr-radius-lg);
  background:
    radial-gradient(circle at top right, rgb(59 130 246 / 0.14), transparent 24%),
    linear-gradient(180deg, rgb(255 255 255 / 0.98), rgb(248 250 252 / 0.94));
}

.page-section__copy {
  display: grid;
  gap: var(--rr-space-3);
  min-width: 0;
}

.page-section__eyebrow {
  margin: 0;
}

.page-section__title-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
  align-items: center;
}

.page-section__title-row h1 {
  margin: 0;
  font-size: clamp(1.95rem, 2.6vw, 2.8rem);
  line-height: 1.02;
  letter-spacing: -0.03em;
}

.page-section__description {
  max-width: 76ch;
  margin: 0;
  color: var(--rr-color-text-secondary);
  font-size: 1.02rem;
}

.page-section__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
  align-items: center;
  justify-content: flex-end;
}

.page-section__body {
  display: grid;
  gap: var(--rr-space-4);
}

@media (width <= 900px) {
  .page-section__header {
    flex-direction: column;
    padding: var(--rr-space-6);
  }

  .page-section__actions {
    justify-content: flex-start;
  }
}
</style>
