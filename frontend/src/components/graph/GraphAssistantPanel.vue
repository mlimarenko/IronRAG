<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import GraphAssistantComposer from 'src/components/graph/assistant/GraphAssistantComposer.vue'
import GraphAssistantHeader from 'src/components/graph/assistant/GraphAssistantHeader.vue'
import GraphAssistantSessionList from 'src/components/graph/assistant/GraphAssistantSessionList.vue'
import GraphAssistantSettingsDialog from 'src/components/graph/assistant/GraphAssistantSettingsDialog.vue'
import GraphAssistantThread from 'src/components/graph/assistant/GraphAssistantThread.vue'
import type { ChatSettingsDraft } from 'src/models/ui/chat'
import type {
  GraphAssistantConfig,
  GraphAssistantState,
  GraphConvergenceStatus,
  GraphQueryMode,
} from 'src/models/ui/graph'

const props = defineProps<{
  assistant: GraphAssistantState
  assistantConfig: GraphAssistantConfig | null
  draft: string
  error: string | null
  submitting: boolean
  sessionLoading: boolean
  sessionError: string | null
  settingsOpen: boolean
  settingsSaving: boolean
  settingsDraft: ChatSettingsDraft | null
  sourceDisclosureState: Record<string, boolean>
  convergenceStatus: GraphConvergenceStatus | null
  activeBlockers: string[]
}>()

const emit = defineEmits<{
  updateDraft: [value: string]
  submit: [question: string]
  selectNode: [id: string]
  createNewChat: []
  loadSession: [sessionId: string]
  openSettings: []
  closeSettings: []
  saveSettings: []
  restoreDefaultSettings: []
  updateSettingsDraftSystemPrompt: [value: string]
  updateSettingsDraftPreferredMode: [value: GraphQueryMode]
  toggleSources: [messageId: string]
}>()

const { t } = useI18n()

const fallbackModes: GraphQueryMode[] = ['document', 'local', 'global', 'hybrid', 'mix']
const fallbackPromptKeys = [
  'graph.defaultPrompts.connectedEntities',
  'graph.defaultPrompts.topEvidence',
  'graph.defaultPrompts.mainThemes',
  'graph.defaultPrompts.isolatedItems',
]

const availableModes = computed(
  () => props.assistantConfig?.modes.map((descriptor) => descriptor.mode) ?? fallbackModes,
)
const promptSuggestions = computed(() =>
  (props.assistantConfig?.defaultPromptKeys ?? fallbackPromptKeys).map((key) => t(key)),
)
const convergenceHint = computed(() => {
  if (props.convergenceStatus === 'partial') {
    return props.activeBlockers[0] ?? t('graph.assistantConvergenceHint.partial')
  }
  if (props.convergenceStatus === 'degraded') {
    return t('graph.assistantConvergenceHint.degraded')
  }
  return null
})
</script>

<template>
  <aside class="rr-graph-assistant rr-graph-assistant--chat">
    <GraphAssistantHeader
      :title="$t('graph.assistantTitle')"
      :subtitle="$t('graph.assistantSubtitle')"
      :active-session="assistant.activeSession"
      :prompt-state="assistant.settingsSummary?.promptState ?? null"
      :busy="submitting || sessionLoading"
      @open-settings="emit('openSettings')"
    />

    <GraphAssistantSessionList
      :sessions="assistant.recentSessions"
      :active-session-id="assistant.activeSession?.sessionId ?? assistant.sessionId"
      :loading="sessionLoading"
      :error="sessionError"
      @select="emit('loadSession', $event)"
      @new-chat="emit('createNewChat')"
    />

    <p
      v-if="convergenceHint"
      class="rr-graph-assistant__hint rr-graph-assistant__hint--subtle"
    >
      {{ convergenceHint }}
    </p>

    <GraphAssistantThread
      :messages="assistant.messages"
      :empty-prompts="promptSuggestions"
      :submitting="submitting"
      :source-disclosure-state="sourceDisclosureState"
      @prompt="emit('submit', $event)"
      @select-node="emit('selectNode', $event)"
      @toggle-sources="emit('toggleSources', $event)"
    />

    <p
      v-if="error"
      class="rr-graph-assistant__error"
    >
      {{ error }}
    </p>

    <GraphAssistantComposer
      :draft="draft"
      :submitting="submitting || sessionLoading"
      @update-draft="emit('updateDraft', $event)"
      @submit="emit('submit', $event)"
    />

    <GraphAssistantSettingsDialog
      :open="settingsOpen"
      :draft="settingsDraft"
      :modes="availableModes"
      :saving="settingsSaving"
      :error="sessionError"
      @update-system-prompt="emit('updateSettingsDraftSystemPrompt', $event)"
      @update-preferred-mode="emit('updateSettingsDraftPreferredMode', $event)"
      @save="emit('saveSettings')"
      @cancel="emit('closeSettings')"
      @restore-default="emit('restoreDefaultSettings')"
    />
  </aside>
</template>
