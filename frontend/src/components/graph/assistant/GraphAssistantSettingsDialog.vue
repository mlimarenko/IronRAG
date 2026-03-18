<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { ChatQueryMode, ChatSettingsDraft } from 'src/models/ui/chat'

defineProps<{
  open: boolean
  draft: ChatSettingsDraft | null
  modes: ChatQueryMode[]
  saving: boolean
  error: string | null
}>()

const emit = defineEmits<{
  updateSystemPrompt: [value: string]
  updatePreferredMode: [value: ChatQueryMode]
  save: []
  cancel: []
  restoreDefault: []
}>()

const { t } = useI18n()
</script>

<template>
  <div
    v-if="open"
    class="rr-assistant-settings"
  >
    <div class="rr-assistant-settings__card">
      <div class="rr-assistant-settings__header">
        <div>
          <strong>{{ t('graph.chat.settingsTitle') }}</strong>
          <p>{{ t('graph.chat.settingsDescription') }}</p>
        </div>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('cancel')"
        >
          ×
        </button>
      </div>

      <p
        v-if="error"
        class="rr-graph-assistant__error rr-graph-assistant__error--inline"
      >
        {{ error }}
      </p>

      <label class="rr-assistant-settings__field">
        <span>{{ t('graph.chat.systemPrompt') }}</span>
        <small>{{ t('graph.chat.systemPromptHelp') }}</small>
        <textarea
          :value="draft?.systemPrompt ?? ''"
          rows="7"
          @input="emit('updateSystemPrompt', ($event.target as HTMLTextAreaElement).value)"
        />
        <small
          v-if="draft?.validationError"
          class="rr-assistant-settings__validation"
        >
          {{ t(`graph.chat.validation.${draft.validationError}`) }}
        </small>
      </label>

      <label class="rr-assistant-settings__field">
        <span>{{ t('graph.chat.preferredMode') }}</span>
        <select
          :value="draft?.preferredMode ?? 'hybrid'"
          @change="
            emit('updatePreferredMode', ($event.target as HTMLSelectElement).value as ChatQueryMode)
          "
        >
          <option
            v-for="mode in modes"
            :key="mode"
            :value="mode"
          >
            {{ mode }}
          </option>
        </select>
      </label>

      <div class="rr-assistant-settings__actions">
        <small
          v-if="draft?.isDirty"
          class="rr-assistant-settings__dirty"
        >
          {{ t('graph.chat.unsavedChanges') }}
        </small>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          :disabled="saving || !draft?.canRestoreDefault"
          @click="emit('restoreDefault')"
        >
          {{ t('graph.chat.restoreDefault') }}
        </button>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          :disabled="saving"
          @click="emit('cancel')"
        >
          {{ t('graph.chat.cancel') }}
        </button>
        <button
          type="button"
          class="rr-button"
          :disabled="saving || !draft?.isDirty || Boolean(draft?.validationError)"
          @click="emit('save')"
        >
          {{ t('graph.chat.save') }}
        </button>
      </div>
    </div>
  </div>
</template>
