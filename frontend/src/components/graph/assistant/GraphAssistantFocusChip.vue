<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { ChatFocusContext } from 'src/models/ui/chat'

defineProps<{
  focus: ChatFocusContext | null
}>()

const emit = defineEmits<{
  remove: []
}>()

const { t } = useI18n()
</script>

<template>
  <div
    v-if="focus"
    class="rr-assistant-focus-chip"
  >
    <div class="rr-assistant-focus-chip__copy">
      <span>{{ t('graph.chat.focusedOn') }}</span>
      <strong>{{ focus.label }}</strong>
      <p>{{ focus.summary }}</p>
    </div>
    <button
      v-if="focus.removable"
      type="button"
      class="rr-button rr-button--ghost rr-button--tiny"
      @click="emit('remove')"
    >
      {{ t('graph.chat.removeFocus') }}
    </button>
  </div>
</template>
