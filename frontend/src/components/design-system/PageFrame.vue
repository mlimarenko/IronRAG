<script setup lang="ts">
withDefaults(
  defineProps<{
    widthMode?: 'default' | 'wide' | 'full'
  }>(),
  {
    widthMode: 'default',
  },
)
</script>

<template>
  <div
    class="rr-page-frame"
    :class="{
      'rr-page-frame--wide': widthMode === 'wide',
      'rr-page-frame--full': widthMode === 'full',
    }"
  >
    <header v-if="$slots.header" class="rr-page-frame__header">
      <slot name="header" />
    </header>

    <section class="rr-page-frame__primary">
      <slot name="primary">
        <slot />
      </slot>
    </section>

    <section v-if="$slots.secondary" class="rr-page-frame__secondary">
      <slot name="secondary" />
    </section>

    <aside v-if="$slots.tertiary" class="rr-page-frame__tertiary">
      <slot name="tertiary" />
    </aside>
  </div>
</template>
