<script setup lang="ts">
import AppPageHeader from 'src/components/ui/AppPageHeader.vue'

withDefaults(
  defineProps<{
    eyebrow?: string
    title: string
    description?: string
    status?: string
    statusLabel?: string
  }>(),
  {
    eyebrow: undefined,
    description: undefined,
    status: undefined,
    statusLabel: undefined,
  },
)
</script>

<template>
  <section class="rr-section">
    <AppPageHeader
      :eyebrow="eyebrow"
      :title="title"
      :description="description"
      :status="status"
      :status-label="statusLabel"
    >
      <template v-if="$slots.actions" #actions>
        <slot name="actions" />
      </template>
    </AppPageHeader>

    <div v-if="$slots.default" class="rr-section__body rr-page-grid">
      <slot />
    </div>
  </section>
</template>
