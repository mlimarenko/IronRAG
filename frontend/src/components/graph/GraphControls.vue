<script setup lang="ts">
import type { GraphLayoutMode } from 'src/models/ui/graph'

defineProps<{
  layoutMode: GraphLayoutMode
  canClearFocus?: boolean
}>()

const emit = defineEmits<{
  zoomIn: []
  zoomOut: []
  fit: []
  setLayout: [value: GraphLayoutMode]
  clearFocus: []
}>()
</script>

<template>
  <div class="rr-graph-controls">
    <div class="rr-graph-controls__layouts">
      <button
        class="rr-button rr-button--ghost rr-button--tiny"
        :class="{ 'is-active': layoutMode === 'cloud' }"
        type="button"
        @click="emit('setLayout', 'cloud')"
      >
        {{ $t('graph.layouts.cloud') }}
      </button>
      <button
        class="rr-button rr-button--ghost rr-button--tiny"
        :class="{ 'is-active': layoutMode === 'rings' }"
        type="button"
        @click="emit('setLayout', 'rings')"
      >
        {{ $t('graph.layouts.rings') }}
      </button>
      <button
        class="rr-button rr-button--ghost rr-button--tiny"
        :class="{ 'is-active': layoutMode === 'lanes' }"
        type="button"
        @click="emit('setLayout', 'lanes')"
      >
        {{ $t('graph.layouts.lanes') }}
      </button>
    </div>
    <button
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      @click="emit('zoomIn')"
    >
      +
    </button>
    <button
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      @click="emit('zoomOut')"
    >
      -
    </button>
    <button
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      @click="emit('fit')"
    >
      {{ $t('graph.fit') }}
    </button>
    <button
      v-if="canClearFocus"
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      @click="emit('clearFocus')"
    >
      {{ $t('graph.clearFocus') }}
    </button>
  </div>
</template>
