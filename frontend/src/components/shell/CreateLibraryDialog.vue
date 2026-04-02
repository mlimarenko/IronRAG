<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useShellStore } from 'src/stores/shell'

defineProps<{
  open: boolean
}>()

const emit = defineEmits<{
  close: []
  submit: [name: string]
}>()

const { t } = useI18n()
const shellStore = useShellStore()
const name = ref('')
const canSubmit = computed(
  () =>
    Boolean(name.value.trim()) &&
    Boolean(shellStore.activeWorkspace) &&
    shellStore.canCreateLibrary,
)

function submit() {
  if (!canSubmit.value) {
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
  <div v-if="open" class="rr-dialog-backdrop" @click.self="close">
    <div class="rr-dialog">
      <h3>{{ t('shell.createLibrary') }}</h3>
      <div class="rr-field">
        <label for="library-name">{{ t('dialogs.name') }}</label>
        <input id="library-name" v-model="name" type="text" @keydown.enter="submit" />
      </div>
      <div class="rr-dialog__actions">
        <button class="rr-button rr-button--ghost" type="button" @click="close">
          {{ t('dialogs.cancel') }}
        </button>
        <button class="rr-button" type="button" :disabled="!canSubmit" @click="submit">
          {{ t('dialogs.create') }}
        </button>
      </div>
    </div>
  </div>
</template>
