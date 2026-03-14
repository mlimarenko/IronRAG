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
    <header class="page-section__header">
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
  gap: var(--rr-space-4);
}

.page-section__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
  padding: var(--rr-space-4) 0 var(--rr-space-5);
  border-bottom: 1px solid var(--rr-color-border-subtle);
}

.page-section__copy {
  display: grid;
  gap: 4px;
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
  font-size: clamp(1.35rem, 2vw, 1.75rem);
  line-height: 1.1;
  letter-spacing: -0.025em;
}

.page-section__description {
  max-width: 48ch;
  margin: 0;
  color: var(--rr-color-text-secondary);
  font-size: 0.9rem;
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
    padding: var(--rr-space-3) 0 var(--rr-space-4);
  }

  .page-section__actions {
    justify-content: flex-start;
  }
}
</style>
