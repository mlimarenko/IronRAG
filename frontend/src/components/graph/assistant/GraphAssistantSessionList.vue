<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { ChatSessionSummary } from 'src/models/ui/chat'

defineProps<{
  sessions: ChatSessionSummary[]
  activeSessionId: string | null
  loading: boolean
  error: string | null
}>()

const emit = defineEmits<{
  select: [sessionId: string]
  newChat: []
}>()

const { t, locale } = useI18n()

function formatRelativeTime(value: string): string {
  const updatedAt = new Date(value).getTime()
  const deltaMinutes = Math.round((updatedAt - Date.now()) / 60_000)

  if (Math.abs(deltaMinutes) < 1) {
    return t('graph.chat.justNow')
  }
  if (Math.abs(deltaMinutes) < 60) {
    return new Intl.RelativeTimeFormat(locale.value, { numeric: 'auto' }).format(
      deltaMinutes,
      'minute',
    )
  }

  const deltaHours = Math.round(deltaMinutes / 60)
  if (Math.abs(deltaHours) < 24) {
    return new Intl.RelativeTimeFormat(locale.value, { numeric: 'auto' }).format(
      deltaHours,
      'hour',
    )
  }

  const deltaDays = Math.round(deltaHours / 24)
  return new Intl.RelativeTimeFormat(locale.value, { numeric: 'auto' }).format(deltaDays, 'day')
}
</script>

<template>
  <section class="rr-assistant-history">
    <button
      type="button"
      class="rr-assistant-history__new"
      @click="emit('newChat')"
    >
      <span class="rr-assistant-history__new-icon">+</span>
      <span>{{ t('graph.chat.newChatAction') }}</span>
    </button>

    <div class="rr-assistant-history__header">
      <strong>{{ t('graph.chat.recentChats') }}</strong>
    </div>

    <p
      v-if="error"
      class="rr-graph-assistant__error rr-graph-assistant__error--inline"
    >
      {{ error }}
    </p>

    <div
      v-if="loading"
      class="rr-assistant-history__empty"
    >
      {{ t('graph.loading') }}
    </div>

    <div
      v-else-if="sessions.length"
      class="rr-assistant-history__list"
    >
      <button
        v-for="session in sessions"
        :key="session.sessionId"
        type="button"
        class="rr-assistant-history__item"
        :class="{ 'is-active': session.sessionId === activeSessionId }"
        @click="emit('select', session.sessionId)"
      >
        <strong>{{ session.isEmpty ? t('graph.chat.newChat') : session.title }}</strong>
        <small>{{ formatRelativeTime(session.updatedAt) }}</small>
      </button>
    </div>

    <div
      v-else
      class="rr-assistant-history__empty"
    >
      {{ t('graph.chat.emptyHistory') }}
    </div>
  </section>
</template>
