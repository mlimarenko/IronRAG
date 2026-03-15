<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import {
  formatReferenceMeta,
  formatReferenceTitle,
  isChunkScopedReference,
} from './statusFormatting'

const props = withDefaults(
  defineProps<{
    title: string
    description?: string
    emptyMessage: string
    references: string[]
  }>(),
  {
    description: undefined,
  },
)

const { t } = useI18n()
const items = computed(() =>
  props.references.map((reference, index) => ({
    id: `${reference}-${String(index)}`,
    title: formatReferenceTitle(reference, index),
    meta: formatReferenceMeta(reference),
    raw: reference,
    isChunkScoped: isChunkScopedReference(reference),
  })),
)
</script>

<template>
  <section class="reference-section rr-panel rr-panel--muted">
    <div class="reference-section__header">
      <div class="reference-section__copy">
        <h4>{{ title }}</h4>
        <p v-if="description" class="rr-note">
          {{ description }}
        </p>
      </div>
      <span v-if="items.length" class="reference-section__count">
        {{ items.length }}
      </span>
    </div>

    <p v-if="!items.length" class="rr-note">
      {{ emptyMessage }}
    </p>

    <ol v-else class="reference-list">
      <li v-for="item in items" :key="item.id" class="reference-card">
        <div class="reference-card__header">
          <div class="reference-card__title-block">
            <strong>{{ item.title }}</strong>
            <p class="reference-card__meta">{{ item.meta }}</p>
          </div>
          <span v-if="item.isChunkScoped" class="reference-card__chip">
            {{ t('flow.search.diagnostics.referenceChip') }}
          </span>
        </div>
        <div class="reference-card__body">
          <span class="reference-card__raw-label">{{
            t('flow.search.result.referenceRawLabel')
          }}</span>
          <code class="reference-card__raw">{{ item.raw }}</code>
        </div>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.reference-section {
  gap: var(--rr-space-4);
}

.reference-section__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.reference-section__copy {
  display: grid;
  gap: 6px;
}

.reference-section__copy h4 {
  margin: 0;
}

.reference-section__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 32px;
  min-height: 32px;
  padding: 0 var(--rr-space-3);
  border-radius: var(--rr-radius-pill);
  background: rgb(29 78 216 / 0.1);
  color: var(--rr-color-accent-700);
  font-size: 0.82rem;
  font-weight: 700;
}

.reference-list {
  display: grid;
  gap: var(--rr-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.reference-card {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.84);
}

.reference-card__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.reference-card__title-block {
  display: grid;
  gap: 6px;
}

.reference-card__header strong {
  font-size: 0.96rem;
}

.reference-card__chip {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 0 var(--rr-space-2);
  border-radius: var(--rr-radius-pill);
  background: rgb(59 130 246 / 0.13);
  color: var(--rr-color-accent-700);
  font-size: 0.74rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.reference-card__meta {
  margin: 0;
  color: var(--rr-color-text-secondary);
}

.reference-card__body {
  display: grid;
  gap: 6px;
}

.reference-card__raw-label {
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.reference-card__raw {
  display: inline-flex;
  align-items: center;
  min-height: 28px;
  width: fit-content;
  max-width: 100%;
  padding: 0 var(--rr-space-2);
  border-radius: calc(var(--rr-radius-sm) - 4px);
  background: rgb(15 23 42 / 0.08);
  color: var(--rr-color-text-primary);
  font-size: 0.82rem;
  overflow-wrap: anywhere;
}

@media (width <= 700px) {
  .reference-section__header,
  .reference-card__header {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
