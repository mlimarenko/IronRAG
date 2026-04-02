<script setup lang="ts">
import { nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  open: boolean
  title: string
  subtitle: string
  submitLabel: string
  submitDisabled?: boolean
  loading?: boolean
}>()

const emit = defineEmits<{
  close: []
  submit: []
}>()

const { t } = useI18n()
const titleId = 'document-action-dialog-title'
const rootRef = ref<HTMLDivElement | null>(null)

watch(
  () => props.open,
  (isOpen) => {
    document.body.style.overflow = isOpen ? 'hidden' : ''
    if (isOpen) {
      void nextTick(() => {
        const focusTarget = rootRef.value?.querySelector<HTMLElement>(
          'input, textarea, select, button:not([disabled])',
        )
        focusTarget?.focus()
      })
    }
  },
)

function close(): void {
  emit('close')
}

function submit(): void {
  if (props.loading || props.submitDisabled) {
    return
  }
  emit('submit')
}
</script>

<template>
  <div
    v-if="props.open"
    class="rr-dialog-backdrop"
    role="dialog"
    aria-modal="true"
    :aria-labelledby="titleId"
    @click.self="close"
    @keydown.escape="close"
  >
    <div ref="rootRef" class="rr-dialog rr-document-dialog rr-document-dialog--frame">
      <header class="rr-document-dialog__header">
        <h3 :id="titleId">{{ props.title }}</h3>
        <p>{{ props.subtitle }}</p>
      </header>

      <div class="rr-document-dialog__body">
        <slot />
      </div>

      <div class="rr-document-dialog__feedback">
        <slot name="feedback" />
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
          class="rr-button"
          type="button"
          :disabled="props.loading || props.submitDisabled"
          @click="submit"
        >
          {{ props.loading ? t('documents.dialogs.submitting') : props.submitLabel }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped lang="scss">
.rr-document-dialog--frame {
  display: grid;
  gap: 16px;
}

.rr-document-dialog__header,
.rr-document-dialog__body,
.rr-document-dialog__feedback {
  display: grid;
  gap: 12px;
}

.rr-document-dialog__header h3,
.rr-document-dialog__header p {
  margin: 0;
}

.rr-document-dialog__header h3 {
  font-size: clamp(1.7rem, 3vw, 2.3rem);
  line-height: 1;
  letter-spacing: -0.05em;
  color: var(--rr-text-primary);
}

.rr-document-dialog__header p {
  color: var(--rr-text-secondary);
  line-height: 1.6;
}

.rr-document-dialog__feedback {
  padding-top: 12px;
  border-top: 1px solid rgba(148, 163, 184, 0.18);
}

.rr-document-dialog__feedback p {
  margin: 0;
}

.rr-document-dialog__feedback:empty {
  display: none;
}

@media (max-width: 720px) {
  .rr-document-dialog--frame {
    gap: 12px;
  }

  .rr-document-dialog__header,
  .rr-document-dialog__body,
  .rr-document-dialog__feedback {
    gap: 10px;
  }

  .rr-document-dialog__header h3 {
    font-size: clamp(1.28rem, 6vw, 1.58rem);
    line-height: 1.08;
    letter-spacing: -0.035em;
  }

  .rr-document-dialog__header p {
    font-size: 0.88rem;
    line-height: 1.5;
  }

  .rr-document-dialog__feedback {
    gap: 6px;
    padding-top: 10px;
  }
}
</style>
