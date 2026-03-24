<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()

const tabs = [
  { to: '/', labelKey: 'shell.home' },
  { to: '/documents', labelKey: 'shell.documents' },
  { to: '/graph', labelKey: 'shell.graph' },
  { to: '/admin', labelKey: 'shell.admin' },
] as const
</script>

<template>
  <nav class="rr-nav-tabs">
    <RouterLink
      v-for="tab in tabs"
      :key="tab.to"
      class="rr-nav-tabs__link"
      :to="tab.to"
      exact-active-class="is-active"
    >
      {{ t(tab.labelKey) }}
    </RouterLink>
  </nav>
</template>
