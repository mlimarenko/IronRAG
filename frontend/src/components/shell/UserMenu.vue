<script setup lang="ts">
defineProps<{
  initials: string
  displayName: string
  accessLabel: string
}>()

const emit = defineEmits<{
  logout: []
}>()
</script>

<template>
  <div class="rr-user-chip">
    <span class="rr-user-chip__avatar">{{ initials }}</span>
    <span class="rr-user-chip__meta">
      <strong>{{ displayName }}</strong>
      <span>{{ accessLabel }}</span>
    </span>
    <button
      class="rr-user-chip__logout"
      type="button"
      @click="emit('logout')"
    >
      ↗
    </button>
  </div>
</template>
