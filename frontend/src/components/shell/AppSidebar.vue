<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

import StatusBadge from './StatusBadge.vue'

const route = useRoute()
const { t } = useI18n()

interface NavItem {
  to: string
  key: 'processing' | 'files' | 'ask' | 'graph' | 'api' | 'context'
}

interface NavGroup {
  label: string
  items: readonly NavItem[]
}

const navGroups = computed<readonly NavGroup[]>(() => [
  {
    label: t('shell.nav.primary'),
    items: [
      { to: '/', key: 'processing' },
      { to: '/ingest', key: 'files' },
      { to: '/ask', key: 'ask' },
      { to: '/graph', key: 'graph' },
      { to: '/api', key: 'api' },
    ],
  },
  {
    label: t('shell.nav.manage'),
    items: [
      { to: '/setup', key: 'context' },
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
      <h1>{{ t('shell.brand.title') }}</h1>
      <StatusBadge :label="t('shell.brand.badge')" tone="info" />
    </div>

    <div class="app-sidebar__groups">
      <nav
        v-for="group in navGroups"
        :key="group.label"
        class="app-sidebar__group"
        :aria-label="group.label"
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
          </RouterLink>
        </div>
      </nav>
    </div>
  </aside>
</template>

<style scoped>
.app-sidebar {
  display: grid;
  gap: var(--rr-space-6);
  align-content: start;
}

.app-sidebar__brand {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--rr-space-3);
}

.app-sidebar__brand h1 {
  margin: 0;
  font-size: 1.15rem;
  line-height: 1.1;
  letter-spacing: -0.02em;
  color: var(--rr-color-text-inverse);
}

.app-sidebar__groups,
.app-sidebar__group,
.app-sidebar__nav {
  display: grid;
  gap: var(--rr-space-2);
}

.app-sidebar__group-label {
  margin: 0 0 4px;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: rgb(148 163 184 / 0.7);
}

.app-sidebar__link {
  display: flex;
  align-items: center;
  min-height: 42px;
  padding: 10px 12px;
  border: 1px solid transparent;
  border-radius: var(--rr-radius-sm);
  color: rgb(203 213 225 / 0.92);
  text-decoration: none;
  background: transparent;
  transition:
    border-color var(--rr-motion-base),
    background var(--rr-motion-base),
    color var(--rr-motion-base);
}

.app-sidebar__link:hover,
.app-sidebar__link[data-active='true'] {
  border-color: rgb(96 165 250 / 0.24);
  background: rgb(148 163 184 / 0.12);
  color: #fff;
}

.app-sidebar__label {
  font-size: 0.94rem;
  font-weight: 600;
}
</style>
