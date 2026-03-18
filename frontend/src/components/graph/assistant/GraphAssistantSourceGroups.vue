<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { AnswerSourceGroup, ChatThreadReference } from 'src/models/ui/chat'

defineProps<{
  groups: AnswerSourceGroup[]
}>()

const emit = defineEmits<{
  inspect: [reference: ChatThreadReference]
}>()

const { t } = useI18n()
</script>

<template>
  <div class="rr-assistant-source-groups">
    <section
      v-for="group in groups"
      :key="group.groupKey"
      class="rr-assistant-source-groups__group"
    >
      <div class="rr-assistant-source-groups__header">
        <span>{{ t(`graph.referenceKinds.${group.groupKey}`) }}</span>
        <small>{{ t('graph.chat.sourceCount', { count: group.itemCount }) }}</small>
      </div>

      <div class="rr-assistant-source-groups__items">
        <button
          v-for="reference in group.items"
          :key="`${reference.kind}:${reference.referenceId}`"
          type="button"
          class="rr-assistant-source-groups__item"
          @click="emit('inspect', reference)"
        >
          {{ reference.excerpt ?? reference.referenceId }}
        </button>
      </div>
    </section>
  </div>
</template>
