<script setup lang="ts">
defineProps<{
  title: string
  emptyMessage: string
  items: string[]
}>()
</script>

<template>
  <div class="section-block rr-panel rr-panel--muted">
    <h4>{{ title }}</h4>
    <p v-if="!items.length" class="muted">
      {{ emptyMessage }}
    </p>
    <ul v-else class="token-list">
      <li v-for="item in items" :key="item">
        <code>{{ item }}</code>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.section-block {
  gap: var(--rr-space-3);
}

.section-block h4 {
  margin: 0;
}

.muted {
  color: var(--rr-color-text-secondary);
}

.token-list {
  display: grid;
  gap: var(--rr-space-2);
  padding-left: 20px;
  margin: 0;
}

.token-list code {
  overflow-wrap: anywhere;
  color: var(--rr-color-text-primary);
}
</style>
