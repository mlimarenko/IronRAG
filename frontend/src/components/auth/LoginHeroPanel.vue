<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <aside class="rr-login__hero">
    <div class="rr-brand">
      <span class="rr-brand__badge">RR</span>
      <span>RustRAG</span>
    </div>
    <div class="rr-login__hero-content">
      <h1>{{ t('auth.heroTitle') }}</h1>
      <p>{{ t('auth.heroBody') }}</p>
    </div>
    <div class="rr-login__hero-footer">{{ t('auth.heroFooter') }}</div>
  </aside>
</template>
