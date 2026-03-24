<script setup lang="ts">
import PageFrame from 'src/components/design-system/PageFrame.vue'

withDefaults(defineProps<{
  mode?: 'default' | 'wide' | 'full'
}>(), {
  mode: 'default',
})
</script>

<template>
  <PageFrame :width-mode="mode">
    <slot />
  </PageFrame>
</template>
