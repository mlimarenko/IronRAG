<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink, useRoute } from 'vue-router'

import StatusBadge from './StatusBadge.vue'

const route = useRoute()

interface NavItem {
  to: string
  label: string
  caption: string
  match?: 'exact'
}

const navItems: readonly NavItem[] = [
  { to: '/', label: 'Overview', caption: 'Start here', match: 'exact' },
  { to: '/setup', label: 'Setup', caption: 'Workspace and project' },
  { to: '/ingest', label: 'Ingest', caption: 'Paste text and index it' },
  { to: '/ask', label: 'Ask', caption: 'Query the indexed content' },
] as const

const activePath = computed(() => route.path)

function isActive(item: NavItem) {
  if (item.match === 'exact') {
    return activePath.value === item.to
  }

  return activePath.value === item.to || activePath.value.startsWith(`${item.to}/`)
}
</script>

<template>
  <aside class="app-sidebar">
    <div class="app-sidebar__brand">
      <div>
        <p class="app-sidebar__eyebrow">RustRAG</p>
        <h1>Document Q&A</h1>
      </div>
      <StatusBadge label="Simple mode" tone="info" emphasis="strong" />
    </div>

    <p class="app-sidebar__summary">
      Four steps: setup, ingest text, ask a question, inspect the answer.
    </p>

    <nav class="app-sidebar__nav" aria-label="Primary">
      <RouterLink
        v-for="item in navItems"
        :key="item.to"
        :to="item.to"
        class="app-sidebar__link"
        :data-active="isActive(item)"
      >
        <span class="app-sidebar__label">{{ item.label }}</span>
        <span class="app-sidebar__caption">{{ item.caption }}</span>
      </RouterLink>
    </nav>
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
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
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
  font-size: 1.45rem;
  line-height: 1.05;
  letter-spacing: -0.03em;
  color: var(--rr-color-text-inverse);
}

.app-sidebar__summary {
  margin: 0;
  max-width: 26ch;
  color: rgb(203 213 225 / 0.86);
  font-size: 0.95rem;
}

.app-sidebar__nav {
  display: grid;
  gap: var(--rr-space-3);
}

.app-sidebar__link {
  display: grid;
  gap: 4px;
  padding: 14px 16px;
  border: 1px solid rgb(148 163 184 / 0.14);
  border-radius: var(--rr-radius-md);
  color: rgb(203 213 225 / 0.94);
  text-decoration: none;
  background:
    linear-gradient(180deg, rgb(15 23 42 / 0.44), rgb(15 23 42 / 0.3)),
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
  border-color: rgb(96 165 250 / 0.45);
  background:
    radial-gradient(circle at right top, rgb(37 99 235 / 0.22), transparent 42%),
    rgb(30 41 59 / 0.92);
  box-shadow: 0 16px 28px rgb(2 6 23 / 0.18);
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
