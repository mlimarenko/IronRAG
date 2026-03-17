<script setup lang="ts">
import { computed, ref, watch } from 'vue'

const props = defineProps<{
  open: boolean
  documentName: string | null
  acceptedFormats: string[]
  loading: boolean
}>()

const emit = defineEmits<{
  close: []
  submit: [file: File]
}>()

const selectedFile = ref<File | null>(null)
const touched = ref(false)

const acceptedFileTypes = computed(() =>
  props.acceptedFormats.map((format) => format.toLowerCase()),
)

const validationError = computed(() => {
  if (!touched.value) {
    return null
  }
  if (!selectedFile.value) {
    return 'required'
  }

  const fileExtension = selectedFile.value.name.split('.').pop()?.toLowerCase()
  if (
    acceptedFileTypes.value.length > 0 &&
    fileExtension &&
    !acceptedFileTypes.value.includes(fileExtension)
  ) {
    return 'type'
  }

  return null
})

watch(
  () => props.open,
  (open) => {
    if (!open) {
      selectedFile.value = null
      touched.value = false
    }
  },
)

function onFileChange(event: Event): void {
  const target = event.target as HTMLInputElement
  selectedFile.value = target.files?.[0] ?? null
  touched.value = true
}

function submit(): void {
  touched.value = true
  if (!selectedFile.value || validationError.value) {
    return
  }

  emit('submit', selectedFile.value)
}
</script>

<template>
  <div
    v-if="props.open"
    class="rr-dialog-backdrop"
    @click.self="emit('close')"
  >
    <div class="rr-dialog rr-document-dialog">
      <h3>{{ $t('documents.dialogs.replace.title') }}</h3>
      <p>{{ $t('documents.dialogs.replace.subtitle', { name: props.documentName ?? '—' }) }}</p>

      <div class="rr-field">
        <label for="replace-document-file">{{ $t('documents.dialogs.replace.fileLabel') }}</label>
        <input
          id="replace-document-file"
          type="file"
          :accept="props.acceptedFormats.map((format) => `.${format}`).join(',')"
          @change="onFileChange"
        >
      </div>

      <p class="rr-document-dialog__hint">
        {{ $t('documents.dialogs.replace.acceptedFormats', { formats: props.acceptedFormats.join(', ') || '—' }) }}
      </p>
      <p
        v-if="selectedFile"
        class="rr-document-dialog__hint"
      >
        {{ $t('documents.dialogs.replace.selectedFile', { name: selectedFile.name }) }}
      </p>
      <p
        v-if="validationError === 'required'"
        class="rr-document-dialog__error"
      >
        {{ $t('documents.dialogs.replace.validationRequired') }}
      </p>
      <p
        v-if="validationError === 'type'"
        class="rr-document-dialog__error"
      >
        {{ $t('documents.dialogs.replace.validationType') }}
      </p>

      <div class="rr-dialog__actions">
        <button
          class="rr-button rr-button--ghost"
          type="button"
          @click="emit('close')"
        >
          {{ $t('dialogs.cancel') }}
        </button>
        <button
          class="rr-button"
          type="button"
          :disabled="props.loading"
          @click="submit"
        >
          {{ props.loading ? $t('documents.dialogs.submitting') : $t('documents.actions.replace') }}
        </button>
      </div>
    </div>
  </div>
</template>
