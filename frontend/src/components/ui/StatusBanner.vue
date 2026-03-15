<script setup lang="ts">
withDefaults(
  defineProps<{
    tone?: 'info' | 'success' | 'warning' | 'danger'
    title?: string
    message?: string
  }>(),
  {
    tone: 'info',
    title: undefined,
    message: undefined,
  },
)
</script>

<template>
  <section class="rr-banner" :data-tone="tone" :role="tone === 'danger' ? 'alert' : 'status'">
    <p v-if="title" class="rr-banner__title">
      {{ title }}
    </p>
    <p v-if="message" class="rr-banner__message">
      {{ message }}
    </p>
    <slot />
  </section>
</template>
