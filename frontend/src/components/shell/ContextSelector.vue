<script setup lang="ts">
import type { LibraryOption, WorkspaceOption } from 'src/models/ui/shell'

defineProps<{
  label: string
  selectedId: string
  options: (WorkspaceOption | LibraryOption)[]
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
      :title="options.find((option) => option.id === selectedId)?.name ?? ''"
      @change="emit('change', ($event.target as HTMLSelectElement).value)"
    >
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
      @click="emit('create')"
    >
      +
    </button>
  </div>
</template>
