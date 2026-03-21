<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DocumentActivityStatus, DocumentStatus } from 'src/models/ui/documents'

const props = defineProps<{
  progressPercent: number | null
  status?: DocumentStatus
  activityStatus?: DocumentActivityStatus
  attemptNo?: number
}>()

const i18n = useI18n()
const { humanizeToken } = useDisplayFormatters()

const activityLabel = computed(() => {
  if (!props.activityStatus) {
    return null
  }
  const key = `documents.activity.${props.activityStatus}`
  return i18n.te(key) ? i18n.t(key) : humanizeToken(props.activityStatus)
})

const progressLabel = computed(() => {
  if (props.progressPercent !== null) {
    return `${String(props.progressPercent)}%`
  }
  if (props.activityStatus === 'blocked' || props.activityStatus === 'retrying' || props.activityStatus === 'stalled') {
    return activityLabel.value
  }
  return '—'
})

const progressTitle = computed(() => {
  const parts = [progressLabel.value]
  if (props.attemptNo) {
    parts.push(i18n.t('documents.attemptShort', { number: props.attemptNo }))
  }
  return parts.join(' · ')
})
</script>

<template>
  <div
    class="rr-progress-cell__stack"
    :title="progressTitle"
  >
    <span
      v-if="props.progressPercent === null"
      class="rr-progress-cell__empty"
      :class="{
        'is-warning':
          props.activityStatus === 'blocked' ||
          props.activityStatus === 'retrying' ||
          props.activityStatus === 'stalled',
      }"
    >{{ progressLabel }}</span>
    <div
      v-else
      class="rr-progress-cell"
      :class="{ 'is-ready-no-graph': props.status === 'ready_no_graph' }"
    >
      <span class="rr-progress-cell__bar">
        <span
          class="rr-progress-cell__fill"
          :style="{ width: `${props.progressPercent}%` }"
        />
      </span>
      <span>{{ progressLabel }}</span>
    </div>
    <span
      v-if="props.attemptNo"
      class="rr-progress-cell__meta"
    >
      {{ $t('documents.attemptShort', { number: props.attemptNo }) }}
    </span>
  </div>
</template>
