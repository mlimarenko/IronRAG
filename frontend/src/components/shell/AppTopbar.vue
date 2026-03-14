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
    workspaceLabel: 'Current workspace',
    projectLabel: 'Current project',
    environmentLabel: 'Ready to work',
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
  gap: var(--rr-space-4);
  align-items: center;
  padding: 18px 22px;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-lg);
  background:
    radial-gradient(circle at top right, rgb(37 99 235 / 0.12), transparent 24%),
    rgb(255 255 255 / 0.84);
  box-shadow: var(--rr-shadow-sm);
}

.app-topbar__context,
.app-topbar__meta {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
  align-items: center;
}

.app-topbar__item {
  display: grid;
  gap: 4px;
  min-width: 160px;
  padding: 10px 14px;
  border: 1px solid rgb(148 163 184 / 0.18);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.56);
}

.app-topbar__item strong {
  color: var(--rr-color-text-primary);
}

.app-topbar__item--compact {
  min-width: auto;
}

.app-topbar__label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

@media (width <= 900px) {
  .app-topbar {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
