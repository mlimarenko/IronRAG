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
  gap: 24px;
  align-content: start;
}

.app-sidebar__brand {
  display: flex;
  justify-content: space-between;
  gap: 16px;
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
  font-size: 1.35rem;
  line-height: 1.1;
}

.app-sidebar__summary {
  margin: 0;
  color: #94a3b8;
  font-size: 0.95rem;
}

.app-sidebar__nav {
  display: grid;
  gap: 10px;
}

.app-sidebar__link {
  display: grid;
  gap: 4px;
  padding: 14px;
  border: 1px solid rgb(148 163 184 / 0.14);
  border-radius: 16px;
  color: #cbd5e1;
  text-decoration: none;
  background: rgb(15 23 42 / 0.32);
  transition:
    transform 120ms ease,
    border-color 120ms ease,
    background 120ms ease;
}

.app-sidebar__link:hover,
.app-sidebar__link[data-active='true'] {
  transform: translateY(-1px);
  border-color: rgb(96 165 250 / 0.45);
  background: rgb(30 41 59 / 0.82);
}

.app-sidebar__label {
  font-weight: 700;
  color: #f8fafc;
}

.app-sidebar__caption {
  font-size: 0.86rem;
  color: #94a3b8;
}
</style>
