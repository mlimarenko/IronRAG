<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import type { ShellSection } from './shellNavigation'
import { getShellNavIndex, shellNavItems } from './shellNavigation'

const props = defineProps<{
  activeSection: ShellSection
}>()

const { t } = useI18n()

const activeIndex = computed(() => getShellNavIndex(props.activeSection))
const previousItem = computed(() => shellNavItems[activeIndex.value - 1] ?? null)
const nextItem = computed(() => shellNavItems[activeIndex.value + 1] ?? null)
const relatedItems = computed(() =>
  shellNavItems
    .filter((item) => item.key !== props.activeSection && item.stage === 'advanced')
    .slice(0, 2),
)
</script>

<template>
  <article class="cross-surface-guide rr-panel rr-panel--muted">
    <div class="cross-surface-guide__header">
      <div>
        <p class="rr-kicker">{{ t('shell.guide.eyebrow') }}</p>
        <h3>{{ t(`shell.pages.${activeSection}.title`) }}</h3>
        <p class="rr-note">{{ t(`shell.guide.sections.${activeSection}.why`) }}</p>
      </div>
      <span class="cross-surface-guide__stage">{{
        t(`shell.guide.sections.${activeSection}.stage`)
      }}</span>
    </div>

    <div class="cross-surface-guide__grid">
      <section class="cross-surface-guide__block">
        <p class="cross-surface-guide__label">{{ t('shell.guide.previous') }}</p>
        <RouterLink v-if="previousItem" :to="previousItem.to" class="cross-surface-guide__link">
          <strong>{{ t(`shell.nav.items.${previousItem.key}.label`) }}</strong>
          <small>{{ t(`shell.guide.sections.${activeSection}.previous`) }}</small>
        </RouterLink>
        <p v-else class="cross-surface-guide__empty">
          {{ t('shell.guide.start') }}
        </p>
      </section>

      <section class="cross-surface-guide__block">
        <p class="cross-surface-guide__label">{{ t('shell.guide.next') }}</p>
        <RouterLink v-if="nextItem" :to="nextItem.to" class="cross-surface-guide__link">
          <strong>{{ t(`shell.nav.items.${nextItem.key}.label`) }}</strong>
          <small>{{ t(`shell.guide.sections.${activeSection}.next`) }}</small>
        </RouterLink>
        <p v-else class="cross-surface-guide__empty">
          {{ t('shell.guide.end') }}
        </p>
      </section>

      <section class="cross-surface-guide__block cross-surface-guide__block--related">
        <p class="cross-surface-guide__label">{{ t('shell.guide.related') }}</p>
        <div class="cross-surface-guide__related-list">
          <RouterLink
            v-for="item in relatedItems"
            :key="item.key"
            :to="item.to"
            class="cross-surface-guide__related-link"
          >
            <strong>{{ t(`shell.nav.items.${item.key}.label`) }}</strong>
            <small>{{ t(`shell.nav.items.${item.key}.hint`) }}</small>
          </RouterLink>
        </div>
      </section>
    </div>
  </article>
</template>

<style scoped>
.cross-surface-guide {
  gap: var(--rr-space-4);
}

.cross-surface-guide__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.cross-surface-guide__header h3 {
  margin: 0;
}

.cross-surface-guide__stage {
  display: inline-flex;
  align-items: center;
  min-height: 30px;
  padding: 0 12px;
  border-radius: 999px;
  background: rgb(15 23 42 / 0.08);
  font-size: 0.78rem;
  font-weight: 700;
  color: var(--rr-color-text-secondary);
}

.cross-surface-guide__grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--rr-space-3);
}

.cross-surface-guide__block {
  display: grid;
  gap: 10px;
  padding: 14px;
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.7);
  border: 1px solid rgb(15 23 42 / 0.06);
}

.cross-surface-guide__label {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.cross-surface-guide__link,
.cross-surface-guide__related-link {
  display: grid;
  gap: 4px;
  color: inherit;
  text-decoration: none;
}

.cross-surface-guide__link strong,
.cross-surface-guide__related-link strong {
  color: var(--rr-color-text-primary);
}

.cross-surface-guide__link small,
.cross-surface-guide__related-link small,
.cross-surface-guide__empty {
  color: var(--rr-color-text-secondary);
}

.cross-surface-guide__related-list {
  display: grid;
  gap: 12px;
}

@media (width <= 900px) {
  .cross-surface-guide__grid {
    grid-template-columns: 1fr;
  }

  .cross-surface-guide__header {
    flex-direction: column;
  }
}
</style>
