<script setup lang="ts">
import StatusBadge from './StatusBadge.vue'

withDefaults(
  defineProps<{
    workspaceLabel?: string
    projectLabel?: string
    environmentLabel?: string
    environmentStatus?: string
  }>(),
  {
    workspaceLabel: 'Default workspace',
    projectLabel: 'Project not selected',
    environmentLabel: 'API boundary ready',
    environmentStatus: 'Healthy',
  },
)
</script>

<template>
  <header class="app-topbar">
    <div class="app-topbar__context">
      <div class="app-topbar__item">
        <span class="app-topbar__label">Workspace</span>
        <strong>{{ workspaceLabel }}</strong>
      </div>
      <div class="app-topbar__item">
        <span class="app-topbar__label">Project</span>
        <strong>{{ projectLabel }}</strong>
      </div>
    </div>

    <div class="app-topbar__meta">
      <div class="app-topbar__item app-topbar__item--compact">
        <span class="app-topbar__label">Status</span>
        <StatusBadge
          :status="environmentStatus"
          :label="environmentLabel"
        />
      </div>
    </div>
  </header>
</template>

<style scoped>
.app-topbar {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: center;
  padding: 16px 20px;
  border: 1px solid #d7dee7;
  border-radius: 20px;
  background: rgb(255 255 255 / 0.88);
  box-shadow: 0 10px 30px rgb(15 23 42 / 0.05);
}

.app-topbar__context,
.app-topbar__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
}

.app-topbar__item {
  display: grid;
  gap: 4px;
  min-width: 160px;
}

.app-topbar__item strong {
  color: #0f172a;
}

.app-topbar__item--compact {
  min-width: auto;
}

.app-topbar__label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: #64748b;
}

@media (width <= 900px) {
  .app-topbar {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
