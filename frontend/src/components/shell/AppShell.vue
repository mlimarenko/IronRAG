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
    workspaceLabel: meta.workspaceLabel ?? 'Default workspace',
    projectLabel: meta.projectLabel ?? 'Project not selected',
    environmentLabel: meta.environmentLabel ?? 'API boundary ready',
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
  color: #0f172a;
}

.app-shell__sidebar {
  padding: 28px 22px;
  background:
    radial-gradient(circle at top, rgb(59 130 246 / 0.22), transparent 35%),
    linear-gradient(180deg, #0f172a 0%, #111827 100%);
}

.app-shell__main {
  display: grid;
  gap: 20px;
  align-content: start;
  padding: 24px;
  background:
    radial-gradient(circle at top right, rgb(59 130 246 / 0.07), transparent 24%),
    linear-gradient(180deg, #f8fafc 0%, #eef4fb 100%);
}

.app-shell__content {
  display: grid;
  gap: 20px;
}

@media (width <= 1100px) {
  .app-shell {
    grid-template-columns: 1fr;
  }
}
</style>
