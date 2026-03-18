<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  canAppend: boolean
  canReplace: boolean
  canRetry: boolean
  canRemove: boolean
  detailAvailable: boolean
  activityStatus: string
  mutationKind: string | null
  mutationStatus: string | null
}>()

const emit = defineEmits<{
  detail: []
  append: []
  replace: []
  retry: []
  remove: []
}>()

const { t } = useI18n()

const mutationLocked = computed(
  () => props.mutationStatus === 'accepted' || props.mutationStatus === 'reconciling',
)
const activityLocked = computed(
  () =>
    props.activityStatus === 'blocked' ||
    props.activityStatus === 'retrying' ||
    props.activityStatus === 'stalled',
)
const retryDisabled = computed(
  () => mutationLocked.value || props.activityStatus === 'retrying' || props.activityStatus === 'blocked',
)
const retryLabel = computed(() => {
  if (props.activityStatus === 'retrying') {
    return t('documents.actions.retrying')
  }
  if (props.activityStatus === 'blocked') {
    return t('documents.actions.waiting')
  }
  if (props.activityStatus === 'stalled') {
    return t('documents.actions.retryStalled')
  }
  return t('documents.actions.retry')
})

function removeLabel(): string {
  if (
    props.mutationKind === 'delete' &&
    (props.mutationStatus === 'accepted' || props.mutationStatus === 'reconciling')
  ) {
    return t('documents.actions.removing')
  }
  return t('documents.actions.remove')
}
</script>

<template>
  <div class="rr-row-actions">
    <button
      v-if="props.detailAvailable"
      class="rr-row-actions__button"
      type="button"
      :title="$t('documents.actions.details')"
      :aria-label="$t('documents.actions.details')"
      @click.stop="emit('detail')"
    >
      <svg
        viewBox="0 0 16 16"
        aria-hidden="true"
      >
        <path
          d="M1.2 8s2.5-4 6.8-4 6.8 4 6.8 4-2.5 4-6.8 4S1.2 8 1.2 8Z"
          fill="none"
          stroke="currentColor"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.4"
        />
        <circle
          cx="8"
          cy="8"
          r="2.2"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
        />
      </svg>
    </button>
    <button
      v-if="props.canAppend"
      class="rr-row-actions__button"
      type="button"
      :disabled="mutationLocked || activityLocked"
      :title="$t('documents.actions.append')"
      :aria-label="$t('documents.actions.append')"
      @click.stop="emit('append')"
    >
      <svg
        viewBox="0 0 16 16"
        aria-hidden="true"
      >
        <path
          d="M8 3v10M3 8h10"
          fill="none"
          stroke="currentColor"
          stroke-linecap="round"
          stroke-width="1.6"
        />
      </svg>
    </button>
    <button
      v-if="props.canReplace"
      class="rr-row-actions__button"
      type="button"
      :disabled="mutationLocked || activityLocked"
      :title="$t('documents.actions.replace')"
      :aria-label="$t('documents.actions.replace')"
      @click.stop="emit('replace')"
    >
      <svg
        viewBox="0 0 16 16"
        aria-hidden="true"
      >
        <path
          d="M3 5.2h7.5M10 3l2.5 2.2L10 7.5M13 10.8H5.5M6 8.5 3.5 10.8 6 13"
          fill="none"
          stroke="currentColor"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.4"
        />
      </svg>
    </button>
    <button
      v-if="props.canRetry"
      class="rr-row-actions__button"
      type="button"
      :disabled="retryDisabled"
      :title="retryLabel"
      :aria-label="retryLabel"
      @click.stop="emit('retry')"
    >
      <svg
        viewBox="0 0 16 16"
        aria-hidden="true"
      >
        <path
          d="M13 8a5 5 0 1 1-1.5-3.55M13 2.8v3.3H9.7"
          fill="none"
          stroke="currentColor"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.4"
        />
      </svg>
    </button>
    <button
      v-if="props.canRemove || props.mutationKind === 'delete'"
      class="rr-row-actions__button is-danger"
      type="button"
      :disabled="mutationLocked || activityLocked"
      :title="removeLabel()"
      :aria-label="removeLabel()"
      @click.stop="emit('remove')"
    >
      <svg
        viewBox="0 0 16 16"
        aria-hidden="true"
      >
        <path
          d="M3.5 4.5h9M6.2 2.8h3.6M5 4.5v7.3m3-7.3v7.3m3-7.3v7.3M4.6 13.2h6.8a.8.8 0 0 0 .8-.74l.44-7.96H3.36l.44 7.96a.8.8 0 0 0 .8.74Z"
          fill="none"
          stroke="currentColor"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.3"
        />
      </svg>
    </button>
  </div>
</template>
