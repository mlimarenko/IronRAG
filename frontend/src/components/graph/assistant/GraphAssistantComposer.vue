<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  draft: string
  submitting: boolean
}>()

const emit = defineEmits<{
  updateDraft: [value: string]
  submit: [value: string]
}>()

const { t } = useI18n()

function submit(): void {
  if (!props.draft.trim()) {
    return
  }
  emit('submit', props.draft.trim())
}
</script>

<template>
  <footer class="rr-assistant-composer">
    <textarea
      :value="draft"
      rows="4"
      :placeholder="t('graph.askPlaceholder')"
      @input="emit('updateDraft', ($event.target as HTMLTextAreaElement).value)"
      @keydown.enter.exact.prevent="submit"
    />

    <div class="rr-assistant-composer__bar">
      <button
        type="button"
        class="rr-assistant-composer__send"
        :disabled="submitting"
        :aria-label="t('graph.ask')"
        @click="submit"
      >
        <svg
          viewBox="0 0 20 20"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M3.333 10 16.667 3.333l-2.5 13.334-4.167-4.167-3.334.833L8.333 10 3.333 10Z"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.6"
          />
        </svg>
      </button>
    </div>
  </footer>
</template>
