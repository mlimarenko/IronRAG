<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import { useAppLocale } from 'src/composables/useAppLocale'

const { t } = useI18n()
const { locale, localeOptions, setLocale } = useAppLocale()

withDefaults(
  defineProps<{
    sectionLabel?: string
    sectionSummary?: string
  }>(),
  {
    sectionLabel: undefined,
    sectionSummary: undefined,
  },
)
</script>

<template>
  <header class="app-topbar">
    <div class="app-topbar__copy">
      <p class="app-topbar__label">{{ t('shell.topbar.surface') }}</p>
      <p class="app-topbar__section">{{ sectionLabel }}</p>
      <p v-if="sectionSummary" class="app-topbar__summary">{{ sectionSummary }}</p>
    </div>

    <div class="app-topbar__controls">
      <div class="app-topbar__locale">
        <span class="app-topbar__locale-label">{{ t('shell.topbar.languageHint') }}</span>
        <div class="rr-segmented" role="group" :aria-label="t('shell.topbar.language')">
          <button
            v-for="option in localeOptions"
            :key="option"
            type="button"
            class="rr-segmented__button"
            :data-active="locale === option"
            :aria-pressed="locale === option"
            :title="t(`common.localeName.${option}`)"
            @click="setLocale(option)"
          >
            {{ t(`shell.locale.${option}`) }}
          </button>
        </div>
      </div>
    </div>
  </header>
</template>

<style scoped>
.app-topbar {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: center;
  padding: 4px 0 2px;
}

.app-topbar__copy,
.app-topbar__controls {
  display: flex;
  gap: var(--rr-space-3);
  align-items: center;
  min-width: 0;
}

.app-topbar__copy {
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
}

.app-topbar__label {
  margin: 0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-topbar__section {
  margin: 0;
  font-family: var(--rr-font-display);
  font-size: 1.2rem;
  font-weight: 700;
  color: var(--rr-color-text-primary);
}

.app-topbar__summary {
  margin: 0;
  color: var(--rr-color-text-secondary);
  font-size: 0.9rem;
}

.app-topbar__locale {
  display: grid;
  gap: 6px;
  justify-items: end;
}

.app-topbar__locale-label {
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

@media (width <= 900px) {
  .app-topbar {
    align-items: flex-start;
    padding-top: 0;
  }

  .app-topbar__controls {
    width: 100%;
    justify-content: flex-end;
  }

  .app-topbar__locale {
    justify-items: start;
  }
}
</style>
