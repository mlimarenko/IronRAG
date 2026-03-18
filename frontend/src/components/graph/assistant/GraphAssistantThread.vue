<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { ChatThreadMessage } from 'src/models/ui/chat'
import GraphAssistantMessageCard from './GraphAssistantMessageCard.vue'

defineProps<{
  messages: ChatThreadMessage[]
  emptyPrompts: string[]
  submitting: boolean
  sourceDisclosureState: Record<string, boolean>
}>()

const emit = defineEmits<{
  prompt: [value: string]
  selectNode: [nodeId: string]
  toggleSources: [messageId: string]
}>()

const { t } = useI18n()
</script>

<template>
  <section class="rr-assistant-thread">
    <div
      v-if="messages.length"
      class="rr-assistant-thread__messages"
    >
      <GraphAssistantMessageCard
        v-for="message in messages"
        :key="message.id"
        :message="message"
        :disclosure-open="Boolean(sourceDisclosureState[message.id])"
        @select-node="emit('selectNode', $event)"
        @toggle-sources="emit('toggleSources', $event)"
      />
    </div>

    <div
      v-else
      class="rr-assistant-thread__empty"
    >
      <p>{{ t('graph.assistantEmpty') }}</p>
      <button
        v-for="prompt in emptyPrompts.slice(0, 3)"
        :key="prompt"
        type="button"
        class="rr-assistant-thread__prompt"
        @click="emit('prompt', prompt)"
      >
        {{ prompt }}
      </button>
    </div>

    <div
      v-if="submitting && !messages.some((message) => message.pending)"
      class="rr-assistant-thread__typing"
    >
      {{ t('graph.chat.thinking') }}
    </div>
  </section>
</template>
