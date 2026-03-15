<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'

import AppMobileNav from './AppMobileNav.vue'
import AppSidebar from './AppSidebar.vue'
import AppTopbar from './AppTopbar.vue'
import type { ShellSection } from './shellNavigation'

const route = useRoute()
const { t } = useI18n()

const routeMeta = computed(() => {
  const meta = route.meta as {
    shellSection?: ShellSection
  }
  const section = meta.shellSection ?? 'home'

  return {
    section,
    sectionLabel: t(`shell.pages.${section}.title`),
    sectionSummary: t(`shell.pages.${section}.summary`),
  }
})
</script>

<template>
  <div class="app-shell">
    <aside class="app-shell__sidebar">
      <AppSidebar />
    </aside>

    <div class="app-shell__main">
      <AppTopbar
        :section-label="routeMeta.sectionLabel"
        :section-summary="routeMeta.sectionSummary"
        :section-key="routeMeta.section"
      />

      <main class="app-shell__content">
        <router-view />
      </main>
    </div>

    <AppMobileNav />
  </div>
</template>

<style scoped>
.app-shell {
  display: grid;
  grid-template-columns: minmax(228px, 248px) minmax(0, 1fr);
  min-height: 100vh;
  color: var(--rr-color-text-primary);
}

.app-shell__sidebar {
  position: sticky;
  top: 0;
  align-self: start;
  min-height: 100vh;
  padding: 24px 16px;
  border-right: 1px solid var(--rr-color-border-subtle);
  background:
    linear-gradient(180deg, rgb(255 255 255 / 0.94), rgb(247 248 243 / 0.96)),
    var(--rr-color-bg-surface-strong);
}

.app-shell__main {
  display: grid;
  gap: var(--rr-space-4);
  align-content: start;
  padding: 24px 28px 32px;
}

.app-shell__content {
  display: grid;
  gap: var(--rr-space-5);
  width: min(100%, 1180px);
}

@media (width <= 1100px) {
  .app-shell {
    grid-template-columns: 1fr;
  }

  .app-shell__sidebar {
    position: static;
    min-height: auto;
    padding: 16px;
    border-right: 0;
    border-bottom: 1px solid var(--rr-color-border-subtle);
  }

  .app-shell__main {
    padding: 18px 16px 24px;
  }
}

@media (width <= 900px) {
  .app-shell__sidebar {
    display: none;
  }

  .app-shell__main {
    min-width: 0;
    padding: 14px 14px calc(88px + env(safe-area-inset-bottom, 0px));
  }

  .app-shell__content {
    width: 100%;
  }
}
</style>
