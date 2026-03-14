<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import StatusBadge from './StatusBadge.vue'

const route = useRoute()
const { t } = useI18n()

interface NavItem {
  to: string
  key: 'overview' | 'workspace' | 'library' | 'search'
}

interface NavGroup {
  label: string
  items: readonly NavItem[]
}

const navGroups = computed<readonly NavGroup[]>(() => [
  {
    label: t('shell.nav.primary'),
    items: [
      { to: '/', key: 'overview' },
      { to: '/ask', key: 'search' },
    ],
  },
  {
    label: t('shell.nav.manage'),
    items: [
      { to: '/setup', key: 'workspace' },
      { to: '/ingest', key: 'library' },
    ],
  },
])

const activePath = computed(() => route.path)

function isActive(item: NavItem) {
  if (item.to === '/') {
    return activePath.value === item.to
  }

  return activePath.value === item.to || activePath.value.startsWith(`${item.to}/`)
}
</script>

<template>
  <aside class="app-sidebar">
    <div class="app-sidebar__brand">
      <div class="app-sidebar__brand-copy">
        <p class="app-sidebar__eyebrow">{{ t('shell.brand.eyebrow') }}</p>
        <h1>{{ t('shell.brand.title') }}</h1>
        <p class="app-sidebar__subtitle">{{ t('shell.brand.subtitle') }}</p>
      </div>
      <StatusBadge :label="t('shell.brand.badge')" tone="info" emphasis="strong" />
    </div>

    <div class="app-sidebar__groups">
      <nav
        v-for="group in navGroups"
        :key="group.label"
        class="app-sidebar__group"
        aria-label="Primary"
      >
        <p class="app-sidebar__group-label">{{ group.label }}</p>
        <div class="app-sidebar__nav">
          <RouterLink
            v-for="item in group.items"
            :key="item.to"
            :to="item.to"
            class="app-sidebar__link"
            :data-active="isActive(item)"
          >
            <span class="app-sidebar__label">{{ t(`shell.nav.items.${item.key}.label`) }}</span>
            <span class="app-sidebar__caption">{{ t(`shell.nav.items.${item.key}.caption`) }}</span>
          </RouterLink>
        </div>
      </nav>
    </div>
  </aside>
</template>

<style scoped>
.app-sidebar {
  display: grid;
  gap: var(--rr-space-7);
  align-content: start;
}

.app-sidebar__brand {
  display: grid;
  gap: var(--rr-space-4);
}

.app-sidebar__brand-copy {
  display: grid;
  gap: 10px;
}

.app-sidebar__eyebrow {
  margin: 0 0 4px;
  font-size: 0.8rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgb(191 219 254 / 0.88);
}

.app-sidebar__brand h1 {
  margin: 0;
  font-size: 1.5rem;
  line-height: 1.05;
  letter-spacing: -0.03em;
  color: var(--rr-color-text-inverse);
}

.app-sidebar__subtitle {
  margin: 0;
  max-width: 24ch;
  color: rgb(203 213 225 / 0.8);
  font-size: 0.92rem;
}

.app-sidebar__groups,
.app-sidebar__group,
.app-sidebar__nav {
  display: grid;
  gap: var(--rr-space-3);
}

.app-sidebar__group-label {
  margin: 0;
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgb(148 163 184 / 0.82);
}

.app-sidebar__link {
  display: grid;
  gap: 3px;
  padding: 13px 15px;
  border: 1px solid rgb(148 163 184 / 0.12);
  border-radius: var(--rr-radius-md);
  color: rgb(203 213 225 / 0.94);
  text-decoration: none;
  background:
    linear-gradient(180deg, rgb(15 23 42 / 0.44), rgb(15 23 42 / 0.28)),
    rgb(15 23 42 / 0.32);
  transition:
    transform var(--rr-motion-base),
    border-color var(--rr-motion-base),
    background var(--rr-motion-base),
    box-shadow var(--rr-motion-base);
}

.app-sidebar__link:hover,
.app-sidebar__link[data-active='true'] {
  transform: translateY(-1px);
  border-color: rgb(96 165 250 / 0.34);
  background:
    radial-gradient(circle at right top, rgb(37 99 235 / 0.18), transparent 42%),
    rgb(30 41 59 / 0.88);
  box-shadow: 0 16px 28px rgb(2 6 23 / 0.14);
}

.app-sidebar__label {
  font-weight: 700;
  color: var(--rr-color-text-inverse);
}

.app-sidebar__caption {
  font-size: 0.86rem;
  color: rgb(148 163 184 / 0.88);
}
</style>
