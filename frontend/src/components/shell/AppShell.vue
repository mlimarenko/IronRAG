<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'

import AppSidebar from './AppSidebar.vue'
import AppTopbar from './AppTopbar.vue'

const route = useRoute()

const routeMeta = computed(() => {
  const meta = route.meta as {
    workspaceLabel?: string
    projectLabel?: string
    environmentLabel?: string
    environmentStatus?: string
  }

  return {
    workspaceLabel: meta.workspaceLabel ?? 'Current workspace',
    projectLabel: meta.projectLabel ?? 'Current project',
    environmentLabel: meta.environmentLabel ?? 'Ready to work',
    environmentStatus: meta.environmentStatus ?? 'Healthy',
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
        :workspace-label="routeMeta.workspaceLabel"
        :project-label="routeMeta.projectLabel"
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
  grid-template-columns: minmax(260px, 300px) minmax(0, 1fr);
  min-height: 100vh;
  color: var(--rr-color-text-primary);
}

.app-shell__sidebar {
  position: sticky;
  top: 0;
  align-self: start;
  min-height: 100vh;
  padding: 28px 22px;
  border-right: 1px solid rgb(148 163 184 / 0.12);
  background:
    radial-gradient(circle at top, rgb(59 130 246 / 0.2), transparent 34%),
    linear-gradient(180deg, #10203a 0%, #0f172a 100%);
}

.app-shell__main {
  display: grid;
  gap: var(--rr-space-5);
  align-content: start;
  padding: 28px;
  background:
    radial-gradient(circle at top right, rgb(59 130 246 / 0.07), transparent 24%),
    linear-gradient(180deg, rgb(255 255 255 / 0.08), rgb(255 255 255 / 0.02));
}

.app-shell__content {
  display: grid;
  gap: var(--rr-space-5);
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
    padding: 22px;
  }
}
</style>
