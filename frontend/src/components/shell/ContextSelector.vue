<script setup lang="ts">
import type { LibraryOption, WorkspaceOption } from 'src/models/ui/shell'

defineProps<{
  label: string
  selectedId: string
  options: (WorkspaceOption | LibraryOption)[]
  disabled?: boolean
  placeholder?: string
  canCreate?: boolean
  createLabel?: string
}>()

const emit = defineEmits<{
  change: [value: string]
  create: []
}>()
</script>

<template>
  <div class="rr-selector">
    <span class="rr-selector__label">{{ label }}</span>
    <select
      :value="selectedId"
      :disabled="disabled"
      :title="options.find((option) => option.id === selectedId)?.name ?? ''"
      @change="emit('change', ($event.target as HTMLSelectElement).value)"
    >
      <option
        v-if="!options.length"
        value=""
        disabled
      >
        {{ placeholder ?? label }}
      </option>
      <option
        v-for="option in options"
        :key="option.id"
        :value="option.id"
      >
        {{ option.name }}
      </option>
    </select>
    <button
      class="rr-selector__add"
      type="button"
      :disabled="!canCreate"
      :aria-label="createLabel ?? label"
      :title="createLabel ?? label"
      @click="emit('create')"
    >
      +
    </button>
  </div>
</template>
