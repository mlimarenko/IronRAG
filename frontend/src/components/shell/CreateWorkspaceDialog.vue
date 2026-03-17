<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{
  open: boolean
}>()

const emit = defineEmits<{
  close: []
  submit: [name: string]
}>()

const { t } = useI18n()
const name = ref('')

function submit() {
  if (!name.value.trim()) {
    return
  }
  emit('submit', name.value.trim())
  name.value = ''
}

function close() {
  name.value = ''
  emit('close')
}
</script>

<template>
  <div
    v-if="open"
    class="rr-dialog-backdrop"
    @click.self="close"
  >
    <div class="rr-dialog">
      <h3>{{ t('shell.createWorkspace') }}</h3>
      <div class="rr-field">
        <label for="workspace-name">{{ t('dialogs.name') }}</label>
        <input
          id="workspace-name"
          v-model="name"
          type="text"
        >
      </div>
      <div class="rr-dialog__actions">
        <button
          class="rr-button rr-button--ghost"
          type="button"
          @click="close"
        >
          {{ t('dialogs.cancel') }}
        </button>
        <button
          class="rr-button"
          type="button"
          @click="submit"
        >
          {{ t('dialogs.create') }}
        </button>
      </div>
    </div>
  </div>
</template>
