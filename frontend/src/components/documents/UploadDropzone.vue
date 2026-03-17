<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
}>()

const emit = defineEmits<{
  select: [files: File[]]
}>()

const { t } = useI18n()
const inputRef = ref<HTMLInputElement | null>(null)

function openPicker() {
  inputRef.value?.click()
}

function emitFiles(fileList: FileList | null) {
  if (!fileList || fileList.length === 0) {
    return
  }
  emit('select', Array.from(fileList))
}
</script>

<template>
  <section
    class="rr-page-card rr-documents__upload"
    :class="{ 'is-loading': props.loading }"
    @click="openPicker"
    @dragover.prevent
    @drop.prevent="emitFiles($event.dataTransfer?.files ?? null)"
  >
    <input
      ref="inputRef"
      class="rr-documents__file-input"
      type="file"
      multiple
      @change="emitFiles(($event.target as HTMLInputElement).files)"
    >
    <p>
      {{ t('documents.upload') }}
      <strong>{{ t('documents.select') }}</strong>
    </p>
    <p>
      {{ props.acceptedFormats.join(', ') }}
      {{ t('documents.maxSize', { size: props.maxSizeMb }) }}
    </p>
    <p class="rr-documents__upload-hint">
      {{ t('documents.uploadQueuedHint') }}
    </p>
  </section>
</template>
