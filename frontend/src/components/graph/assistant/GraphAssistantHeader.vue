<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { ChatPromptState, ChatSessionDetail } from 'src/models/ui/chat'

defineProps<{
  title: string
  subtitle: string
  activeSession: ChatSessionDetail | null
  promptState: ChatPromptState | null
  busy: boolean
}>()

const emit = defineEmits<{
  openSettings: []
}>()

const { t } = useI18n()
</script>

<template>
  <header class="rr-assistant-header">
    <div class="rr-assistant-header__top">
      <div class="rr-assistant-header__eyebrow">
        <span>{{ title }}</span>
        <span
          v-if="promptState"
          class="rr-assistant-header__badge"
          :class="`is-${promptState}`"
        >
          {{ t(`graph.chat.${promptState === 'default' ? 'defaultBadge' : 'customizedBadge'}`) }}
        </span>
      </div>
      <button
        type="button"
        class="rr-assistant-header__settings"
        :disabled="busy"
        :aria-label="t('graph.chat.settingsButton')"
        @click="emit('openSettings')"
      >
        <svg
          viewBox="0 0 20 20"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M10 6.75a3.25 3.25 0 1 0 0 6.5 3.25 3.25 0 0 0 0-6.5Z"
            stroke="currentColor"
            stroke-width="1.5"
          />
          <path
            d="M16.25 10a6.49 6.49 0 0 0-.08-.99l1.44-1.13-1.5-2.6-1.75.54a6.6 6.6 0 0 0-1.7-.99l-.3-1.8H9.64l-.3 1.8c-.6.2-1.17.53-1.7.99l-1.75-.54-1.5 2.6 1.44 1.13a6.84 6.84 0 0 0 0 1.98L4.39 12.12l1.5 2.6 1.75-.54c.53.45 1.1.78 1.7.98l.3 1.81h2.72l.3-1.8c.6-.21 1.17-.54 1.7-.99l1.75.54 1.5-2.6-1.44-1.13c.05-.32.08-.65.08-.99Z"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.3"
          />
        </svg>
      </button>
    </div>

    <div class="rr-assistant-header__copy">
      <h3>{{ activeSession?.isEmpty ? t('graph.chat.newChat') : (activeSession?.title ?? title) }}</h3>
      <p>{{ subtitle }}</p>
    </div>
  </header>
</template>
