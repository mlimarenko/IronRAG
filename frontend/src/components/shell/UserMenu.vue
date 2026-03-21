<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  initials: string
  displayName: string
  accessLabel: string
}>()

const emit = defineEmits<{
  logout: []
}>()

const { t } = useI18n()

const localizedAccessLabel = computed(() => {
  switch (props.accessLabel) {
    case 'Admin access':
      return t('shell.access.admin')
    case 'Write access':
      return t('shell.access.write')
    case 'Read access':
      return t('shell.access.read')
    default:
      return props.accessLabel
  }
})
</script>

<template>
  <div class="rr-user-chip">
    <span class="rr-user-chip__avatar">{{ initials }}</span>
    <span class="rr-user-chip__meta">
      <strong>{{ displayName }}</strong>
      <span>{{ localizedAccessLabel }}</span>
    </span>
    <button
      class="rr-user-chip__logout"
      type="button"
      @click="emit('logout')"
    >
      ↗
    </button>
  </div>
</template>
