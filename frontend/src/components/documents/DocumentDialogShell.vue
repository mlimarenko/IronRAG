<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'

const props = withDefaults(defineProps<{
  open: boolean
  title: string
  description: string
  submitLabel: string
  loading?: boolean
  submitDisabled?: boolean
  tone?: 'default' | 'danger'
}>(), {
  loading: false,
  submitDisabled: false,
  tone: 'default',
})

const emit = defineEmits<{
  close: []
  submit: []
}>()

const dialogRef = ref<HTMLElement | null>(null)
const titleId = computed(() => `document-dialog-title-${props.title.toLowerCase().replace(/\s+/g, '-')}`)

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) {
      document.body.style.overflow = 'hidden'
      void nextTick(() => dialogRef.value?.focus())
      return
    }
    document.body.style.overflow = ''
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  document.body.style.overflow = ''
})
</script>

<template>
  <div
    v-if="props.open"
    class="rr-dialog-backdrop"
    role="dialog"
    aria-modal="true"
    :aria-labelledby="titleId"
    @click.self="emit('close')"
    @keydown.escape="emit('close')"
  >
    <div
      ref="dialogRef"
      class="rr-dialog rr-document-dialog"
      :class="{ 'rr-dialog--danger': props.tone === 'danger' }"
      tabindex="-1"
    >
      <div class="rr-document-dialog__header">
        <h3 :id="titleId">{{ props.title }}</h3>
        <p>{{ props.description }}</p>
      </div>

      <div class="rr-document-dialog__body">
        <slot />
      </div>

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
          :class="{ 'rr-button--danger': props.tone === 'danger' }"
          type="button"
          :disabled="props.submitDisabled || props.loading"
          @click="emit('submit')"
        >
          {{ props.submitLabel }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-document-dialog {
  width: min(520px, calc(100vw - 32px));
  display: grid;
  gap: 1rem;
}

.rr-document-dialog__header,
.rr-document-dialog__body {
  display: grid;
  gap: 0.75rem;
}

.rr-document-dialog__header h3,
.rr-document-dialog__header p {
  margin: 0;
}

.rr-document-dialog__header h3 {
  font-size: clamp(1.5rem, 3vw, 1.95rem);
  line-height: 1;
  letter-spacing: -0.04em;
}

.rr-document-dialog__header p {
  color: rgba(15, 23, 42, 0.64);
  font-size: 0.95rem;
  line-height: 1.55;
}

@media (max-width: 720px) {
  .rr-document-dialog {
    width: min(100%, 32rem);
    gap: 0.85rem;
  }

  .rr-document-dialog__header,
  .rr-document-dialog__body {
    gap: 0.65rem;
  }

  .rr-document-dialog__header h3 {
    font-size: clamp(1.22rem, 5.8vw, 1.52rem);
    line-height: 1.08;
    letter-spacing: -0.03em;
  }

  .rr-document-dialog__header p {
    font-size: 0.86rem;
    line-height: 1.5;
  }
}
</style>
