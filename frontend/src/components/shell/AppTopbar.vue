<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import { useAppLocale } from 'src/composables/useAppLocale'

const { t } = useI18n()
const { locale, localeOptions, setLocale } = useAppLocale()

withDefaults(
  defineProps<{
    sectionLabel?: string
    environmentLabel?: string
    environmentStatus?: string
  }>(),
  {
    sectionLabel: undefined,
    environmentLabel: undefined,
    environmentStatus: 'ready',
  },
)
</script>

<template>
  <header class="app-topbar">
    <div class="app-topbar__copy">
      <p class="app-topbar__section">{{ sectionLabel }}</p>
    </div>

    <div class="app-topbar__controls">
      <div class="rr-segmented" role="group" :aria-label="t('shell.topbar.language')">
        <button
          v-for="option in localeOptions"
          :key="option"
          type="button"
          class="rr-segmented__button"
          :data-active="locale === option"
          :aria-pressed="locale === option"
          @click="setLocale(option)"
        >
          {{ t(`shell.locale.${option}`) }}
        </button>
      </div>
    </div>
  </header>
</template>

<style scoped>
.app-topbar {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
  padding: 12px 16px;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(255 255 255 / 0.78);
}

.app-topbar__copy,
.app-topbar__controls {
  display: flex;
  gap: var(--rr-space-3);
  align-items: center;
  min-width: 0;
}

.app-topbar__section {
  margin: 0;
  font-size: 0.95rem;
  font-weight: 700;
  color: var(--rr-color-text-primary);
}

@media (width <= 900px) {
  .app-topbar {
    flex-direction: column;
    align-items: stretch;
  }

  .app-topbar__controls {
    justify-content: flex-start;
  }
}
</style>
