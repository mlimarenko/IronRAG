<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  buildDocumentUploadAcceptString,
  formatAcceptedDocumentFormats,
} from 'src/models/ui/documentFormats'

const props = withDefaults(
  defineProps<{
    acceptedFormats: string[]
    maxSizeMb: number
    loading: boolean
    variant?: 'inline' | 'panel'
    showMeta?: boolean
  }>(),
  {
    variant: 'inline',
    showMeta: true,
  },
)

const emit = defineEmits<{
  select: [files: File[]]
}>()

const { t } = useI18n()
const inputRef = ref<HTMLInputElement | null>(null)
const isDragging = ref(false)
const acceptString = computed(() => buildDocumentUploadAcceptString(props.acceptedFormats))
const isInline = computed(() => props.variant === 'inline')
const acceptedFormatsLabel = computed(() =>
  formatAcceptedDocumentFormats(props.acceptedFormats, (format) =>
    t(`documents.fileFormats.${format}`),
  ),
)
const compactFormatsLabel = computed(() => {
  const formats = acceptedFormatsLabel.value.split(', ').filter(Boolean)
  const visibleFormats = formats.slice(0, 6)
  const hiddenCount = props.acceptedFormats.length - visibleFormats.length
  const prefix = visibleFormats.join(', ')
  return hiddenCount > 0 ? `${prefix} +${hiddenCount}` : prefix
})
const metaLabel = computed(() => {
  if (isInline.value && !props.showMeta) {
    return ''
  }
  return isInline.value
    ? compactFormatsLabel.value
    : `${acceptedFormatsLabel.value} · ${t('documents.maxSize', { size: props.maxSizeMb })}`
})

function openPicker() {
  inputRef.value?.click()
}

function onDragEnter(e: DragEvent) {
  e.preventDefault()
  isDragging.value = true
}

function onDragLeave(_e: DragEvent) {
  isDragging.value = false
}

function onDrop(e: DragEvent) {
  isDragging.value = false
  emitFiles(e.dataTransfer?.files ?? null)
}

function emitFiles(fileList: FileList | null) {
  if (!fileList || fileList.length === 0) {
    return
  }
  emit('select', Array.from(fileList))
}
</script>

<template>
  <div
    class="rr-upload-dropzone"
    :class="{
      'is-loading': props.loading,
      'is-inline': isInline,
      'is-panel': !isInline,
      'is-dragging': isDragging,
    }"
    @dragover.prevent
    @dragenter="onDragEnter"
    @dragleave="onDragLeave"
    @drop.prevent="onDrop"
  >
    <input
      ref="inputRef"
      class="rr-documents__file-input"
      type="file"
      multiple
      hidden
      tabindex="-1"
      aria-hidden="true"
      :accept="acceptString"
      @change="emitFiles(($event.target as HTMLInputElement).files)"
    />
    <button
      type="button"
      class="rr-button rr-button--primary rr-button--compact rr-upload-dropzone__button"
      :title="isInline ? metaLabel : undefined"
      @click="openPicker"
    >
      {{ isInline ? t('documents.uploadCta') : t('documents.uploadOnboardingCta') }}
    </button>
    <div v-if="!isInline || props.showMeta" class="rr-upload-dropzone__copy">
      <p v-if="!isInline" class="rr-upload-dropzone__title">
        {{ t('documents.uploadOnboardingTitle') }}
      </p>
      <p v-if="!isInline" class="rr-upload-dropzone__lead">
        {{ t('documents.uploadOnboardingDescription') }}
      </p>
      <p class="rr-upload-dropzone__meta">
        {{ metaLabel }}
      </p>
      <p v-if="!isInline" class="rr-upload-dropzone__hint">
        {{ t('documents.uploadQueuedHint') }}
      </p>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-upload-dropzone {
  display: inline-flex;
  width: 100%;
  align-items: center;
  gap: 0.8rem;
  border: 1px solid var(--rr-border-soft);
  border-radius: var(--rr-radius-lg);
  background: rgba(248, 250, 252, 0.92);
  transition:
    border-color 180ms ease,
    box-shadow 180ms ease,
    opacity 180ms ease,
    transform 180ms ease;
}

.rr-upload-dropzone:hover {
  border-color: color-mix(in srgb, var(--rr-accent) 40%, var(--rr-border-soft));
  box-shadow: 0 0 0 0.22rem color-mix(in srgb, var(--rr-accent) 10%, transparent);
}

.rr-upload-dropzone.is-dragging {
  border-color: var(--rr-accent);
  background: rgba(99, 102, 241, 0.04);
}

.rr-upload-dropzone.is-loading {
  opacity: 0.72;
}

.rr-upload-dropzone.is-inline {
  width: auto;
  min-width: 0;
  gap: 0.4rem;
  padding: 0;
  border: none;
  background: transparent;
  box-shadow: none;
}

.rr-upload-dropzone.is-inline:hover {
  border-color: transparent;
  box-shadow: none;
}

.rr-upload-dropzone.is-panel {
  width: min(100%, 30rem);
  min-width: min(100%, 18rem);
  padding: 1rem 1.05rem;
  align-items: flex-start;
}

.rr-upload-dropzone__button {
  flex: none;
}

.rr-upload-dropzone__copy {
  display: grid;
  gap: 0.16rem;
  min-width: 0;
}

.rr-upload-dropzone__title {
  margin: 0;
  font-size: 0.75rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-text-muted);
}

.rr-upload-dropzone__lead {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
  line-height: 1.4;
}

.rr-upload-dropzone__meta,
.rr-upload-dropzone__hint {
  margin: 0;
  line-height: 1.45;
}

.rr-upload-dropzone__meta {
  font-size: 0.82rem;
  color: var(--rr-text-secondary);
}

.rr-upload-dropzone__hint {
  font-size: 0.76rem;
  color: var(--rr-text-muted);
}

.rr-upload-dropzone.is-inline .rr-upload-dropzone__title,
.rr-upload-dropzone.is-inline .rr-upload-dropzone__lead,
.rr-upload-dropzone.is-inline .rr-upload-dropzone__hint {
  display: none;
}

.rr-upload-dropzone.is-inline .rr-upload-dropzone__copy {
  gap: 0;
}

.rr-upload-dropzone.is-inline .rr-upload-dropzone__meta {
  font-size: 0.56rem;
  line-height: 1.25;
}

@media (max-width: 920px) {
  .rr-upload-dropzone.is-inline {
    width: 100%;
    justify-content: space-between;
    padding: 0.3rem 0.34rem;
    border-radius: 14px;
  }
}

@media (max-width: 720px) {
  .rr-upload-dropzone {
    min-width: 0;
    width: 100%;
    flex-direction: column;
    align-items: flex-start;
  }

  .rr-upload-dropzone__button {
    width: 100%;
    justify-content: center;
  }
}

@media (max-width: 600px) {
  .rr-upload-dropzone.is-inline {
    min-width: 100%;
    gap: 0;
    padding: 0;
    border: none;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
  }

  .rr-upload-dropzone.is-inline:hover {
    border-color: transparent;
    box-shadow: none;
  }

  .rr-upload-dropzone.is-inline .rr-upload-dropzone__button {
    min-height: 2.5rem;
  }

  .rr-upload-dropzone.is-inline .rr-upload-dropzone__meta {
    display: none;
  }
}
</style>
