<script setup lang="ts">
withDefaults(defineProps<{
  tone?: 'default' | 'muted' | 'accent'
  density?: 'comfortable' | 'compact'
  padded?: boolean
}>(), {
  tone: 'default',
  density: 'comfortable',
  padded: true,
})
</script>

<template>
  <section
    class="rr-surface-panel"
    :class="[
      `rr-surface-panel--${tone}`,
      { 'rr-surface-panel--unpadded': !padded, 'rr-surface-panel--compact': density === 'compact' },
    ]"
  >
    <slot />
  </section>
</template>
