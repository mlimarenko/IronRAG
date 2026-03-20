<script setup lang="ts">
import { useI18n } from 'vue-i18n'
</script>

<template>
  <nav class="rr-nav-tabs">
    <RouterLink
      class="rr-nav-tabs__link"
      to="/documents"
    >
      {{ useI18n().t('shell.documents') }}
    </RouterLink>
    <RouterLink
      class="rr-nav-tabs__link"
      to="/graph"
    >
      {{ useI18n().t('shell.graph') }}
    </RouterLink>
    <RouterLink
      class="rr-nav-tabs__link"
      to="/swagger"
    >
      {{ useI18n().t('shell.swagger') }}
    </RouterLink>
    <RouterLink
      class="rr-nav-tabs__link"
      to="/admin"
    >
      {{ useI18n().t('shell.admin') }}
    </RouterLink>
  </nav>
</template>
