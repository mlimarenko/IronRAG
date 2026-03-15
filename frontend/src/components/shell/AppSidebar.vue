<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import { shellNavItems, type ShellNavItem } from './shellNavigation'

const route = useRoute()
const { t } = useI18n()

interface NavGroup {
  key: 'primary' | 'advanced'
  items: readonly ShellNavItem[]
}

const navGroups = computed<readonly NavGroup[]>(() => [
  {
    key: 'primary',
    items: shellNavItems
      .map((item, index) => ({ ...item, stepLabel: String(index + 1).padStart(2, '0') }))
      .filter((item) => item.stage === 'primary'),
  },
  {
    key: 'advanced',
    items: shellNavItems
      .map((item, index) => ({ ...item, stepLabel: String(index + 1).padStart(2, '0') }))
      .filter((item) => item.stage === 'advanced'),
  },
])

const activePath = computed(() => route.path)
const activeSection = computed(
  () =>
    (route.meta.shellSection as ShellNavItem['key'] | undefined) ??
    shellNavItems.find(
      (item) => activePath.value === item.to || activePath.value.startsWith(`${item.to}/`),
    )?.key ??
    'documents',
)

function isActive(item: ShellNavItem) {
  return (
    activePath.value === item.to ||
    activePath.value.startsWith(`${item.to}/`) ||
    Boolean(
      item.legacyTo &&
      (activePath.value === item.legacyTo || activePath.value.startsWith(`${item.legacyTo}/`)),
    )
  )
}
</script>

<template>
  <aside class="app-sidebar">
    <div class="app-sidebar__brand">
      <RouterLink to="/" class="app-sidebar__brand-link">
        <span class="app-sidebar__brand-mark">R</span>
        <div class="app-sidebar__brand-copy">
          <h1>{{ t('shell.brand.title') }}</h1>
          <p>{{ t('shell.brand.subtitle') }}</p>
        </div>
      </RouterLink>
    </div>

    <div class="app-sidebar__focus">
      <p class="app-sidebar__focus-label">{{ t('shell.topbar.surface') }}</p>
      <strong>{{ t(`shell.nav.items.${activeSection}.label`) }}</strong>
      <small>{{ t(`shell.nav.items.${activeSection}.hint`) }}</small>
    </div>

    <div class="app-sidebar__sections" :aria-label="t('shell.nav.product')">
      <section v-for="group in navGroups" :key="group.key" class="app-sidebar__section">
        <p class="app-sidebar__section-label">{{ t(`shell.nav.groups.${group.key}`) }}</p>
        <nav class="app-sidebar__nav" :aria-label="t(`shell.nav.groups.${group.key}`)">
          <RouterLink
            v-for="item in group.items"
            :key="item.to"
            :to="item.to"
            class="app-sidebar__link"
            :data-active="isActive(item)"
            :aria-current="isActive(item) ? 'page' : undefined"
          >
            <span class="app-sidebar__step">{{ item.step }}</span>
            <span class="app-sidebar__label">{{ t(`shell.nav.items.${item.key}.label`) }}</span>
          </RouterLink>
        </nav>
      </section>
    </div>
  </aside>
</template>

<style scoped>
.app-sidebar {
  display: grid;
  gap: var(--rr-space-6);
  align-content: start;
}

.app-sidebar__brand-link {
  display: inline-flex;
  align-items: center;
  gap: 12px;
  color: inherit;
  text-decoration: none;
}

.app-sidebar__brand-mark {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 12px;
  background: var(--rr-color-bg-contrast);
  color: var(--rr-color-text-inverse);
  font-family: var(--rr-font-display);
  font-size: 1rem;
  font-weight: 700;
}

.app-sidebar__brand-copy {
  display: grid;
  gap: 2px;
}

.app-sidebar__focus {
  display: grid;
  gap: 4px;
  padding: 12px 14px;
  border: 1px solid rgb(15 23 42 / 0.06);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.66);
}

.app-sidebar__focus-label {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-sidebar__focus strong,
.app-sidebar__focus small {
  margin: 0;
}

.app-sidebar__focus small {
  color: var(--rr-color-text-secondary);
}

.app-sidebar__sections {
  display: grid;
  gap: var(--rr-space-5);
}

.app-sidebar__section {
  display: grid;
  gap: var(--rr-space-3);
}

.app-sidebar__section-label {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-sidebar__brand h1 {
  margin: 0;
  font-family: var(--rr-font-display);
  font-size: 1.12rem;
  line-height: 1.1;
  letter-spacing: -0.02em;
  color: var(--rr-color-text-primary);
}

.app-sidebar__brand p {
  margin: 0;
  font-size: 0.9rem;
  color: var(--rr-color-text-secondary);
}

.app-sidebar__nav {
  display: grid;
  gap: 10px;
}

.app-sidebar__link {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 50px;
  padding: 12px 14px;
  border: 1px solid rgb(15 23 42 / 0.06);
  border-radius: calc(var(--rr-radius-md) + 2px);
  color: var(--rr-color-text-secondary);
  text-decoration: none;
  background: rgb(255 255 255 / 0.54);
  box-shadow: 0 10px 28px rgb(15 23 42 / 0.03);
  transition:
    border-color var(--rr-motion-base),
    background var(--rr-motion-base),
    color var(--rr-motion-base),
    box-shadow var(--rr-motion-base),
    transform var(--rr-motion-base);
}

.app-sidebar__link:hover,
.app-sidebar__link[data-active='true'] {
  border-color: rgb(59 130 246 / 0.18);
  background: var(--rr-color-bg-surface-strong);
  color: var(--rr-color-text-primary);
  transform: translateX(2px);
  box-shadow: 0 14px 30px rgb(59 130 246 / 0.08);
}

.app-sidebar__link:focus-visible {
  outline: 2px solid rgb(59 130 246 / 0.45);
  outline-offset: 2px;
}

.app-sidebar__step {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 38px;
  min-height: 30px;
  padding: 0 10px;
  border-radius: 999px;
  background: rgb(15 23 42 / 0.06);
  font-size: 0.74rem;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: var(--rr-color-text-muted);
}

.app-sidebar__link:hover .app-sidebar__step,
.app-sidebar__link[data-active='true'] .app-sidebar__step {
  background: rgb(59 130 246 / 0.12);
  color: var(--rr-color-accent-700);
}

.app-sidebar__label {
  font-size: 0.92rem;
  font-weight: 650;
}

@media (width <= 1100px) {
  .app-sidebar__sections {
    gap: var(--rr-space-4);
  }

  .app-sidebar__nav {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .app-sidebar__link {
    min-height: 56px;
  }
}

@media (width <= 900px) {
  .app-sidebar__focus,
  .app-sidebar__sections,
  :deep(.product-spine) {
    display: none;
  }
}

@media (width <= 700px) {
  .app-sidebar__nav {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .app-sidebar__link {
    padding: 12px;
  }

  .app-sidebar__step {
    min-width: 34px;
    min-height: 28px;
    padding: 0 8px;
  }
}
</style>
