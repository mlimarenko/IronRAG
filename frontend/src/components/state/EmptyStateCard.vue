<script setup lang="ts">
withDefaults(
  defineProps<{
    title: string
    message: string
    hint?: string
  }>(),
  {
    hint: undefined,
  },
)
</script>

<template>
  <article class="rr-empty-state">
    <div class="rr-empty-state__icon" aria-hidden="true">○</div>
    <div class="rr-empty-state__copy">
      <h3>{{ title }}</h3>
      <p>{{ message }}</p>
      <p v-if="hint" class="rr-empty-state__hint">
        {{ hint }}
      </p>
    </div>
    <div v-if="$slots.actions" class="rr-empty-state__actions">
      <slot name="actions" />
    </div>
  </article>
</template>
