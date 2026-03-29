<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import DocumentActionDialog from './DocumentActionDialog.vue'

const props = defineProps<{
  open: boolean
  documentName: string | null
  loading: boolean
  error?: string | null
}>()

const emit = defineEmits<{
  close: []
  submit: [content: string]
}>()

const content = ref('')
const touched = ref(false)

const canSubmit = computed(() => content.value.trim().length > 0)
const showValidation = computed(() => touched.value && !canSubmit.value)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      content.value = ''
      touched.value = false
    }
  },
)

function submit(): void {
  touched.value = true
  if (!canSubmit.value) {
    return
  }
  emit('submit', content.value.trim())
}
</script>

<template>
  <DocumentActionDialog
    :open="props.open"
    :title="$t('documents.dialogs.append.title')"
    :subtitle="$t('documents.dialogs.append.subtitle', { name: props.documentName ?? '—' })"
    :submit-label="$t('documents.actions.append')"
    :submit-disabled="!canSubmit"
    :loading="props.loading"
    @close="emit('close')"
    @submit="submit"
  >
    <div class="rr-field">
      <label for="append-document-content">{{ $t('documents.dialogs.append.contentLabel') }}</label>
      <textarea
        id="append-document-content"
        v-model="content"
        rows="8"
        :placeholder="$t('documents.dialogs.append.placeholder')"
      />
    </div>

    <template #feedback>
      <p
        v-if="showValidation"
        class="rr-document-dialog__error"
      >
        {{ $t('documents.dialogs.append.validation') }}
      </p>
      <p
        v-else-if="props.error"
        class="rr-document-dialog__error"
      >
        {{ props.error }}
      </p>
    </template>
  </DocumentActionDialog>
</template>
