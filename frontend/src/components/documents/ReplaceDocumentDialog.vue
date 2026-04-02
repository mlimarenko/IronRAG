<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  buildDocumentUploadAcceptString,
  formatAcceptedDocumentFormats,
  isAcceptedDocumentUpload,
} from 'src/models/ui/documentFormats'
import DocumentActionDialog from './DocumentActionDialog.vue'

const props = defineProps<{
  open: boolean
  documentName: string | null
  acceptedFormats: string[]
  loading: boolean
  error?: string | null
}>()

const emit = defineEmits<{
  close: []
  submit: [file: File]
}>()

const { t } = useI18n()
const selectedFile = ref<File | null>(null)
const touched = ref(false)
const acceptString = computed(() => buildDocumentUploadAcceptString(props.acceptedFormats))
const acceptedFormatsLabel = computed(() =>
  formatAcceptedDocumentFormats(props.acceptedFormats, (format) =>
    t(`documents.fileFormats.${format}`),
  ),
)

const validationError = computed(() => {
  if (!touched.value) {
    return null
  }
  if (!selectedFile.value) {
    return 'required'
  }

  if (!isAcceptedDocumentUpload(selectedFile.value, props.acceptedFormats)) {
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
  <DocumentActionDialog
    :open="props.open"
    :title="$t('documents.dialogs.replace.title')"
    :subtitle="$t('documents.dialogs.replace.subtitle', { name: props.documentName ?? '—' })"
    :submit-label="$t('documents.actions.replace')"
    :submit-disabled="Boolean(validationError)"
    :loading="props.loading"
    @close="emit('close')"
    @submit="submit"
  >
    <div class="rr-field">
      <label for="replace-document-file">{{ $t('documents.dialogs.replace.fileLabel') }}</label>
      <input id="replace-document-file" type="file" :accept="acceptString" @change="onFileChange" />
    </div>

    <template #feedback>
      <p class="rr-document-dialog__hint">
        {{ $t('documents.dialogs.replace.acceptedFormats', { formats: acceptedFormatsLabel }) }}
      </p>
      <p v-if="selectedFile" class="rr-document-dialog__hint">
        {{ $t('documents.dialogs.replace.selectedFile', { name: selectedFile.name }) }}
      </p>
      <p v-if="validationError === 'required'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.replace.validationRequired') }}
      </p>
      <p v-if="validationError === 'type'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.replace.validationType') }}
      </p>
      <p v-else-if="props.error" class="rr-document-dialog__error">
        {{ props.error }}
      </p>
    </template>
  </DocumentActionDialog>
</template>
