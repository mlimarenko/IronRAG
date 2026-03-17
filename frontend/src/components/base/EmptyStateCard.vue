<script setup lang="ts">
defineProps<{
  title: string
  description: string
}>()
</script>

<template>
  <div class="rr-page-card rr-empty-card">
    <h3>{{ title }}</h3>
    <p>{{ description }}</p>
  </div>
</template>
