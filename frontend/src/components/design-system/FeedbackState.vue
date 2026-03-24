<script setup lang="ts">
import SurfacePanel from './SurfacePanel.vue'

withDefaults(defineProps<{
  title: string
  message: string
  kind?: 'empty' | 'loading' | 'error' | 'sparse' | 'warning'
  actionLabel?: string
  details?: string[]
}>(), {
  kind: 'empty',
  actionLabel: undefined,
  details: () => [],
})

const emit = defineEmits<{
  (event: 'action'): void
}>()
</script>

<template>
  <SurfacePanel
    class="rr-feedback-card"
    :class="`rr-feedback-card--${kind}`"
  >
    <div class="rr-feedback-card__copy">
      <h3>{{ title }}</h3>
      <p>{{ message }}</p>
      <ul
        v-if="details.length"
        class="rr-feedback-card__details"
      >
        <li
          v-for="detail in details"
          :key="detail"
        >
          {{ detail }}
        </li>
      </ul>
      <div
        v-if="actionLabel || $slots.action"
        class="rr-feedback-card__action"
      >
        <slot name="action">
          <button
            type="button"
            class="rr-button rr-button--primary"
            @click="emit('action')"
          >
            {{ actionLabel }}
          </button>
        </slot>
      </div>
    </div>
  </SurfacePanel>
</template>
