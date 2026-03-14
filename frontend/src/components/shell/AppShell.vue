<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'

import AppSidebar from './AppSidebar.vue'
import AppTopbar from './AppTopbar.vue'

const route = useRoute()
const { t } = useI18n()

type ShellSection = 'processing' | 'setup' | 'files' | 'search' | 'graph' | 'api'

const routeMeta = computed(() => {
  const meta = route.meta as {
    shellSection?: ShellSection
    shellStatus?: 'focused' | 'ready' | 'healthy'
  }
  const section = meta.shellSection ?? 'processing'
  const shellStatus = meta.shellStatus ?? 'ready'

  return {
    sectionLabel: t(`shell.pages.${section}.title`),
    environmentLabel: t(`shell.status.${shellStatus}`),
    environmentStatus: shellStatus,
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
        :environment-label="routeMeta.environmentLabel"
        :environment-status="routeMeta.environmentStatus"
      />

      <main class="app-shell__content">
        <router-view />
      </main>
    </div>
  </div>
</template>

<style scoped>
.app-shell {
  display: grid;
  grid-template-columns: minmax(248px, 284px) minmax(0, 1fr);
  min-height: 100vh;
  color: var(--rr-color-text-primary);
}

.app-shell__sidebar {
  position: sticky;
  top: 0;
  align-self: start;
  min-height: 100vh;
  padding: 26px 20px;
  border-right: 1px solid rgb(120 138 164 / 0.16);
  background:
    radial-gradient(circle at top, rgb(44 93 215 / 0.22), transparent 34%),
    linear-gradient(180deg, #192132 0%, #141b28 100%);
}

.app-shell__main {
  display: grid;
  gap: var(--rr-space-4);
  align-content: start;
  padding: 24px;
  background:
    radial-gradient(circle at top right, rgb(44 93 215 / 0.08), transparent 24%),
    linear-gradient(180deg, rgb(255 255 255 / 0.1), rgb(255 255 255 / 0.04));
}

.app-shell__content {
  display: grid;
  gap: var(--rr-space-4);
}

@media (width <= 1100px) {
  .app-shell {
    grid-template-columns: 1fr;
  }

  .app-shell__sidebar {
    position: static;
    min-height: auto;
    border-right: 0;
    border-bottom: 1px solid rgb(148 163 184 / 0.12);
  }

  .app-shell__main {
    padding: 18px;
  }
}
</style>
