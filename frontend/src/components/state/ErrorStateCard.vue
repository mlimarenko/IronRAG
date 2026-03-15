<script setup lang="ts">
withDefaults(
  defineProps<{
    title?: string
    message: string
    detail?: string
  }>(),
  {
    title: 'Something broke',
    detail: undefined,
  },
)
</script>

<template>
  <article class="rr-empty-state rr-empty-state--danger" role="alert">
    <div class="rr-empty-state__icon" aria-hidden="true">
      !
    </div>
    <div class="rr-empty-state__copy">
      <h3>{{ title }}</h3>
      <p>{{ message }}</p>
      <p v-if="detail" class="rr-empty-state__hint">
        {{ detail }}
      </p>
    </div>
    <div v-if="$slots.actions" class="rr-empty-state__actions">
      <slot name="actions" />
    </div>
  </article>
</template>
