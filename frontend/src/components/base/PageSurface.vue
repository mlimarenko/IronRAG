<script setup lang="ts">
withDefaults(defineProps<{
  wide?: boolean
}>(), {
  wide: false,
})
</script>

<template>
  <div
    class="rr-page-surface"
    :class="{ 'rr-page-surface--wide': wide }"
  >
    <slot />
  </div>
</template>
