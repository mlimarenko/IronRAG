<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink, useRoute } from 'vue-router'

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
      { to: '/setup', key: 'context' },
      { to: '/ingest', key: 'files' },
      { to: '/ask', key: 'ask' },
    ],
  },
  {
    label: t('shell.nav.manage'),
    items: [
      { to: '/graph', key: 'graph' },
      { to: '/api', key: 'api' },
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
      <RouterLink to="/" class="app-sidebar__brand-link">
        <span class="app-sidebar__brand-mark">R</span>
        <div class="app-sidebar__brand-copy">
          <h1>{{ t('shell.brand.title') }}</h1>
          <p>{{ t('shell.brand.subtitle') }}</p>
        </div>
      </RouterLink>
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
  gap: var(--rr-space-7);
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

.app-sidebar__groups,
.app-sidebar__group,
.app-sidebar__nav {
  display: grid;
  gap: var(--rr-space-2);
}

.app-sidebar__group-label {
  margin: 0 0 6px;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-sidebar__link {
  display: flex;
  align-items: center;
  min-height: 44px;
  padding: 10px 12px;
  border: 1px solid transparent;
  border-radius: calc(var(--rr-radius-sm) + 2px);
  color: var(--rr-color-text-secondary);
  text-decoration: none;
  background: rgb(255 255 255 / 0.38);
  transition:
    border-color var(--rr-motion-base),
    background var(--rr-motion-base),
    color var(--rr-motion-base),
    transform var(--rr-motion-base);
}

.app-sidebar__link:hover,
.app-sidebar__link[data-active='true'] {
  border-color: rgb(59 130 246 / 0.16);
  background: var(--rr-color-bg-surface-strong);
  color: var(--rr-color-text-primary);
  transform: translateX(2px);
}

.app-sidebar__label {
  font-size: 0.94rem;
  font-weight: 650;
}
</style>
