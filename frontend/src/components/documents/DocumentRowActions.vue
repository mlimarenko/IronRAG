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
</script>

<template>
  <div class="rr-row-actions">
    <button
      v-if="props.detailAvailable"
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      @click.stop="emit('detail')"
    >
      {{ $t('documents.actions.details') }}
    </button>
    <button
      v-if="props.canAppend"
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      :disabled="mutationLocked || activityLocked"
      @click.stop="emit('append')"
    >
      {{ $t('documents.actions.append') }}
    </button>
    <button
      v-if="props.canReplace"
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      :disabled="mutationLocked || activityLocked"
      @click.stop="emit('replace')"
    >
      {{ $t('documents.actions.replace') }}
    </button>
    <button
      v-if="props.canRetry"
      class="rr-button rr-button--ghost rr-button--tiny"
      type="button"
      :disabled="retryDisabled"
      @click.stop="emit('retry')"
    >
      {{ retryLabel }}
    </button>
    <button
      v-if="props.canRemove || props.mutationKind === 'delete'"
      class="rr-button rr-button--ghost rr-button--tiny is-danger"
      type="button"
      :disabled="mutationLocked || activityLocked"
      @click.stop="emit('remove')"
    >
      {{
        props.mutationKind === 'delete' &&
          (props.mutationStatus === 'accepted' || props.mutationStatus === 'reconciling')
          ? $t('documents.actions.removing')
          : $t('documents.actions.remove')
      }}
    </button>
  </div>
</template>
