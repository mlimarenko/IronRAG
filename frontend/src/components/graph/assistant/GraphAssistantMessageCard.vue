<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ChatThreadMessage, ChatThreadReference } from 'src/models/ui/chat'
import GraphAssistantSourceGroups from './GraphAssistantSourceGroups.vue'

const props = defineProps<{
  message: ChatThreadMessage
  disclosureOpen: boolean
}>()

const emit = defineEmits<{
  selectNode: [nodeId: string]
  toggleSources: [messageId: string]
}>()

const { t } = useI18n()
const sourceCount = computed(() =>
  props.message.sourceGroups.reduce((total, group) => total + group.itemCount, 0),
)

function normalizeReferenceExcerpt(value: string | null): string | null {
  if (!value) {
    return null
  }

  const normalized = value
    .replace(/<[^>]+>/g, ' ')
    .replace(/&[a-z]+;/gi, ' ')
    .replace(/\[[^\]]*]\([^)]+\)/g, ' ')
    .replace(/https?:\/\/\S+/gi, ' ')
    .replace(/[`*_>#-]+/g, ' ')
    .replace(/[│├┤┬┴─\\]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()

  return normalized || null
}

function referenceLabel(reference: ChatThreadReference, index: number): string {
  return (
    normalizeReferenceExcerpt(reference.excerpt) ??
    (!/^[0-9a-f-]{30,}$/i.test(reference.referenceId)
      ? reference.referenceId
      : `${t('graph.referenceFallback')} #${String(index + 1)}`)
  )
}

function inspectReference(reference: ChatThreadReference): void {
  if (reference.kind === 'node') {
    emit('selectNode', reference.referenceId)
  }
}
</script>

<template>
  <article
    class="rr-assistant-message"
    :class="`is-${message.role}`"
  >
    <div class="rr-assistant-message__avatar">
      {{ message.role === 'user' ? t('graph.youShort') : t('graph.assistant') }}
    </div>

    <div class="rr-assistant-message__body">
      <div
        class="rr-assistant-message__bubble"
        :class="{ 'is-pending': message.pending }"
      >
        <p
          v-if="message.pending"
          class="rr-assistant-message__pending"
        >
          {{ t('graph.chat.thinking') }}
        </p>
        <p>{{ message.content }}</p>
      </div>

      <div
        v-if="
          message.role === 'assistant' &&
            (message.mode || message.groundingStatus || message.provider)
        "
        class="rr-assistant-message__meta"
      >
        <span v-if="message.mode">{{ message.mode }}</span>
        <span v-if="message.groundingStatus">{{ message.groundingStatus }}</span>
        <span v-if="message.provider">
          {{ message.provider.providerKind }} · {{ message.provider.modelName }}
        </span>
      </div>

      <p
        v-if="message.warning"
        class="rr-assistant-message__warning"
        :class="message.warningLevel ? `is-${message.warningLevel}` : null"
      >
        <strong>{{ t(`graph.chat.warningState.${message.warningLevel ?? 'info'}`) }}</strong>
        {{ message.warning }}
      </p>

      <div
        v-if="message.role === 'assistant' && message.sourceGroups.length"
        class="rr-assistant-message__sources"
      >
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('toggleSources', message.id)"
        >
          {{ t('graph.groundedSources') }} · {{ t('graph.chat.sourceCount', { count: sourceCount }) }}
        </button>

        <GraphAssistantSourceGroups
          v-if="disclosureOpen"
          :groups="message.sourceGroups.map((group) => ({
            ...group,
            items: group.items.map((reference, index) => ({
              ...reference,
              excerpt: referenceLabel(reference, index),
            })),
          }))"
          @inspect="inspectReference"
        />
      </div>
    </div>
  </article>
</template>
