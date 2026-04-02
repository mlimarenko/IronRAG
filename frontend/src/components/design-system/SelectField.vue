<script setup lang="ts">
export interface SelectFieldOption {
  id: string
  label: string
}

withDefaults(
  defineProps<{
    modelValue: string
    options: SelectFieldOption[]
    disabled?: boolean
  }>(),
  {
    disabled: false,
  },
)

const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void
}>()
</script>

<template>
  <select
    class="rr-field rr-field--select"
    :value="modelValue"
    :disabled="disabled"
    @change="emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
  >
    <option v-for="option in options" :key="option.id" :value="option.id">
      {{ option.label }}
    </option>
  </select>
</template>
