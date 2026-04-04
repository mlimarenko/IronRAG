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
      <div class="rr-login__hero-tags">
        <span class="rr-login__hero-tag">{{ t('auth.heroTags.documents') }}</span>
        <span class="rr-login__hero-tag">{{ t('auth.heroTags.entities') }}</span>
        <span class="rr-login__hero-tag">{{ t('auth.heroTags.graph') }}</span>
        <span class="rr-login__hero-tag">{{ t('auth.heroTags.ai') }}</span>
      </div>
    </div>
    <div class="rr-login__hero-footer">{{ t('auth.heroFooter') }}</div>
  </aside>
</template>
