<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = withDefaults(
  defineProps<{
    title?: string
    message: string
    detail?: string
  }>(),
  {
    title: undefined,
    detail: undefined,
  },
)

const { t } = useI18n()
const resolvedTitle = computed(() => props.title ?? t('errors.somethingBroke'))
</script>

<template>
  <article
    class="rr-empty-state rr-empty-state--danger"
    role="alert"
  >
    <div
      class="rr-empty-state__icon"
      aria-hidden="true"
    >
      !
    </div>
    <div class="rr-empty-state__copy">
      <h3>{{ resolvedTitle }}</h3>
      <p>{{ props.message }}</p>
      <p
        v-if="props.detail"
        class="rr-empty-state__hint"
      >
        {{ props.detail }}
      </p>
    </div>
    <div
      v-if="$slots.actions"
      class="rr-empty-state__actions"
    >
      <slot name="actions" />
    </div>
  </article>
</template>
