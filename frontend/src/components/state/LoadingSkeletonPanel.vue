<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = withDefaults(
  defineProps<{
    lines?: number
    title?: string
  }>(),
  {
    lines: 4,
    title: undefined,
  },
)

const { t } = useI18n()
const resolvedTitle = computed(() => props.title ?? t('common.loading'))
</script>

<template>
  <article
    class="rr-panel rr-skeleton-panel"
    aria-busy="true"
    aria-live="polite"
  >
    <div class="rr-skeleton-panel__header">
      <span class="rr-skeleton-panel__badge">{{ resolvedTitle }}</span>
      <div class="rr-skeleton-panel__line rr-skeleton-panel__line--short" />
    </div>
    <div class="rr-skeleton-panel__body">
      <div
        v-for="index in props.lines"
        :key="index"
        class="rr-skeleton-panel__line"
        :class="{ 'rr-skeleton-panel__line--short': index === props.lines }"
      />
    </div>
  </article>
</template>
