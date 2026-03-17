<script setup lang="ts">
import { computed, ref, watch } from 'vue'

const props = defineProps<{
  open: boolean
  documentName: string | null
  loading: boolean
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
  <div
    v-if="props.open"
    class="rr-dialog-backdrop"
    @click.self="emit('close')"
  >
    <div class="rr-dialog rr-document-dialog">
      <h3>{{ $t('documents.dialogs.append.title') }}</h3>
      <p>{{ $t('documents.dialogs.append.subtitle', { name: props.documentName ?? '—' }) }}</p>

      <div class="rr-field">
        <label for="append-document-content">{{ $t('documents.dialogs.append.contentLabel') }}</label>
        <textarea
          id="append-document-content"
          v-model="content"
          rows="8"
          :placeholder="$t('documents.dialogs.append.placeholder')"
        />
      </div>

      <p
        v-if="showValidation"
        class="rr-document-dialog__error"
      >
        {{ $t('documents.dialogs.append.validation') }}
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
          {{ props.loading ? $t('documents.dialogs.submitting') : $t('documents.actions.append') }}
        </button>
      </div>
    </div>
  </div>
</template>
