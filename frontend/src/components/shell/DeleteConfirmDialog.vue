<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  open: boolean
  title: string
  targetName: string
  warning: string
  confirmLabel?: string
  loading?: boolean
}>()

const emit = defineEmits<{
  close: []
  confirm: []
}>()

const { t } = useI18n()
const confirmInput = ref('')
const inputRef = ref<HTMLInputElement | null>(null)
const titleId = 'delete-confirm-dialog-title'

const canConfirm = computed(() => confirmInput.value.trim() === props.targetName)

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) {
      document.body.style.overflow = 'hidden'
      void nextTick(() => inputRef.value?.focus())
    } else {
      confirmInput.value = ''
      document.body.style.overflow = ''
    }
  },
)

function submit() {
  if (!canConfirm.value || props.loading) return
  emit('confirm')
  confirmInput.value = ''
}

function close() {
  confirmInput.value = ''
  emit('close')
}
</script>

<template>
  <div
    v-if="open"
    class="rr-dialog-backdrop"
    role="dialog"
    aria-modal="true"
    :aria-labelledby="titleId"
    @click.self="close"
    @keydown.escape="close"
  >
    <div class="rr-dialog rr-dialog--danger">
      <h3 :id="titleId">{{ title }}</h3>
      <p class="rr-dialog__warning">
        {{ warning }}
      </p>
      <div class="rr-field">
        <label for="delete-confirm-input">
          {{ t('dialogs.deleteConfirmHint', { name: targetName }) }}
        </label>
        <input
          id="delete-confirm-input"
          ref="inputRef"
          v-model="confirmInput"
          type="text"
          autocomplete="off"
          @keydown.enter="submit"
        />
      </div>
      <div class="rr-dialog__actions">
        <button
          class="rr-button rr-button--ghost"
          type="button"
          :disabled="props.loading"
          @click="close"
        >
          {{ t('dialogs.cancel') }}
        </button>
        <button
          class="rr-button rr-button--danger"
          type="button"
          :disabled="!canConfirm || props.loading"
          @click="submit"
        >
          {{ props.confirmLabel ?? t('dialogs.delete') }}
        </button>
      </div>
    </div>
  </div>
</template>
