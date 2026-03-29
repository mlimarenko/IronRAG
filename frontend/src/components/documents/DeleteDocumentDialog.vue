<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import DocumentDialogShell from './DocumentDialogShell.vue'

const props = defineProps<{
  open: boolean
  documentName: string | null
  loading: boolean
  error?: string | null
}>()

const emit = defineEmits<{
  close: []
  submit: []
}>()

const confirmInput = ref('')

const canSubmit = computed(
  () => confirmInput.value.trim() === (props.documentName ?? ''),
)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      confirmInput.value = ''
    }
  },
)

function submit(): void {
  if (!canSubmit.value) {
    return
  }
  emit('submit')
}
</script>

<template>
  <DocumentDialogShell
    :open="props.open"
    :title="$t('documents.dialogs.delete.title')"
    :description="$t('documents.dialogs.delete.subtitle', { name: props.documentName ?? '—' })"
    :submit-label="props.loading ? $t('documents.actions.removing') : $t('dialogs.delete')"
    :submit-disabled="!canSubmit"
    :loading="props.loading"
    tone="danger"
    @close="emit('close')"
    @submit="submit"
  >
    <p class="rr-dialog__warning">
      {{ $t('documents.dialogs.delete.warning') }}
    </p>

    <div class="rr-field">
      <label for="delete-document-confirm">
        {{ $t('dialogs.deleteConfirmHint', { name: props.documentName ?? '—' }) }}
      </label>
      <input
        id="delete-document-confirm"
        v-model="confirmInput"
        type="text"
        autocomplete="off"
        @keydown.enter="submit"
      >
    </div>

    <p
      v-if="props.error"
      class="rr-document-dialog__error"
    >
      {{ props.error }}
    </p>
  </DocumentDialogShell>
</template>
