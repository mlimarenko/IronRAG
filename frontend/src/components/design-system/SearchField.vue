<script setup lang="ts">
defineProps<{
  modelValue: string
  placeholder?: string
  disabled?: boolean
}>()

const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void
  (event: 'clear'): void
}>()
</script>

<template>
  <div class="rr-search-field">
    <slot name="icon" />
    <input
      :value="modelValue"
      type="search"
      class="rr-field rr-field--search"
      :placeholder="placeholder"
      :disabled="disabled"
      @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
    >
    <button
      v-if="modelValue"
      type="button"
      class="rr-button rr-button--subtle rr-search-field__clear"
      @click="emit('clear')"
    >
      ×
    </button>
  </div>
</template>
