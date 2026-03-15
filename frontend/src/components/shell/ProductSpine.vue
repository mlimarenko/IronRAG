<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import type { ShellSection } from './shellNavigation'
import { shellNavItems } from './shellNavigation'

const props = withDefaults(
  defineProps<{
    activeSection: ShellSection
    compact?: boolean
  }>(),
  {
    compact: false,
  },
)

const { t } = useI18n()

const items = computed(() =>
  shellNavItems.map((item, index) => ({
    ...item,
    index,
    title: t(`shell.nav.items.${item.key}.label`),
    hint: t(`shell.nav.items.${item.key}.hint`),
    active: item.key === props.activeSection,
    complete: index < shellNavItems.findIndex((candidate) => candidate.key === props.activeSection),
  })),
)
</script>

<template>
  <div class="product-spine" :data-compact="compact">
    <div class="product-spine__header">
      <p class="product-spine__eyebrow">{{ t('shell.spine.eyebrow') }}</p>
      <p class="product-spine__summary">{{ t(`shell.pages.${activeSection}.summary`) }}</p>
    </div>

    <ol class="product-spine__list">
      <li
        v-for="item in items"
        :key="item.key"
        class="product-spine__item"
        :data-active="item.active"
        :data-complete="item.complete"
      >
        <RouterLink :to="item.to" class="product-spine__link">
          <span class="product-spine__step">{{ item.step }}</span>
          <span class="product-spine__copy">
            <strong>{{ item.title }}</strong>
            <small>{{ item.hint }}</small>
          </span>
        </RouterLink>
      </li>
    </ol>
  </div>
</template>

<style scoped>
.product-spine {
  display: grid;
  gap: var(--rr-space-4);
  padding: 16px 18px;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: calc(var(--rr-radius-lg) + 2px);
  background: rgb(255 255 255 / 0.74);
  box-shadow: 0 18px 40px rgb(15 23 42 / 0.05);
}

.product-spine__header {
  display: grid;
  gap: 4px;
}

.product-spine__eyebrow {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.product-spine__summary {
  margin: 0;
  font-size: 0.94rem;
  color: var(--rr-color-text-secondary);
}

.product-spine__list {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 12px;
  padding: 0;
  margin: 0;
  list-style: none;
}

.product-spine__item {
  min-width: 0;
}

.product-spine__link {
  display: flex;
  gap: 12px;
  align-items: flex-start;
  min-height: 68px;
  padding: 12px;
  border-radius: var(--rr-radius-md);
  border: 1px solid rgb(15 23 42 / 0.06);
  text-decoration: none;
  color: inherit;
  background: rgb(248 250 252 / 0.72);
}

.product-spine__item[data-active='true'] .product-spine__link {
  border-color: rgb(59 130 246 / 0.24);
  background: rgb(239 246 255 / 0.95);
}

.product-spine__item[data-complete='true'] .product-spine__link {
  background: rgb(240 253 244 / 0.92);
}

.product-spine__step {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 34px;
  min-height: 34px;
  border-radius: 999px;
  background: rgb(15 23 42 / 0.08);
  font-size: 0.74rem;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: var(--rr-color-text-muted);
}

.product-spine__item[data-active='true'] .product-spine__step {
  background: rgb(59 130 246 / 0.14);
  color: var(--rr-color-accent-700);
}

.product-spine__item[data-complete='true'] .product-spine__step {
  background: rgb(34 197 94 / 0.14);
  color: rgb(21 128 61);
}

.product-spine__copy {
  display: grid;
  gap: 4px;
  min-width: 0;
}

.product-spine__copy strong {
  font-size: 0.92rem;
  color: var(--rr-color-text-primary);
}

.product-spine__copy small {
  font-size: 0.78rem;
  line-height: 1.35;
  color: var(--rr-color-text-secondary);
}

.product-spine[data-compact='true'] .product-spine__list {
  grid-template-columns: 1fr;
}

@media (width <= 1100px) {
  .product-spine__list {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (width <= 700px) {
  .product-spine {
    padding: 14px;
  }

  .product-spine__list {
    grid-template-columns: 1fr;
  }
}
</style>
