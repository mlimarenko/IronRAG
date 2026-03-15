<script setup lang="ts">
import { computed, useSlots } from 'vue'

import StatusBadge from 'src/components/shell/StatusBadge.vue'

const props = withDefaults(
  defineProps<{
    tag?: 'article' | 'section' | 'div'
    tone?: 'default' | 'muted' | 'accent'
    eyebrow?: string
    title?: string
    description?: string
    status?: string
    statusLabel?: string
    titleTag?: 'h2' | 'h3' | 'h4'
  }>(),
  {
    tag: 'article',
    tone: 'default',
    eyebrow: undefined,
    title: undefined,
    description: undefined,
    status: undefined,
    statusLabel: undefined,
    titleTag: 'h3',
  },
)

const slots = useSlots()

const panelClass = computed(() => ({
  'rr-panel--muted': props.tone === 'muted',
  'rr-panel--accent': props.tone === 'accent',
}))

const hasHeader = computed(
  () =>
    Boolean(props.eyebrow || props.title || props.description || props.status || props.statusLabel) ||
    Boolean(slots.actions),
)
</script>

<template>
  <component :is="tag" class="rr-panel" :class="panelClass">
    <div v-if="hasHeader" class="rr-panel__header">
      <div class="rr-panel__header-copy">
        <p v-if="eyebrow" class="rr-panel__eyebrow rr-kicker">
          {{ eyebrow }}
        </p>

        <div v-if="title || status || statusLabel" class="rr-panel__title-row">
          <component v-if="title" :is="titleTag" class="rr-panel__title">
            {{ title }}
          </component>

          <StatusBadge
            v-if="status || statusLabel"
            :status="status"
            :label="statusLabel"
          />
        </div>

        <p v-if="description" class="rr-panel__description">
          {{ description }}
        </p>
      </div>

      <div v-if="$slots.actions" class="rr-panel__actions">
        <slot name="actions" />
      </div>
    </div>

    <div v-if="$slots.default" class="rr-panel__body">
      <slot />
    </div>
  </component>
</template>
