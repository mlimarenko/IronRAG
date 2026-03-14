<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import { useAppLocale } from 'src/composables/useAppLocale'

import StatusBadge from './StatusBadge.vue'

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
      <span class="app-topbar__label">{{ t('shell.topbar.surface') }}</span>
      <p class="app-topbar__section">{{ sectionLabel }}</p>
    </div>

    <div class="app-topbar__controls">
      <div class="app-topbar__locale">
        <span class="app-topbar__label">{{ t('shell.topbar.language') }}</span>
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

      <div class="app-topbar__status">
        <span class="app-topbar__label">{{ t('shell.topbar.state') }}</span>
        <StatusBadge
          :status="environmentStatus"
          :label="environmentLabel"
        />
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
  padding: 14px 18px;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background:
    radial-gradient(circle at top right, rgb(44 93 215 / 0.08), transparent 22%),
    rgb(255 255 255 / 0.76);
  box-shadow: var(--rr-shadow-sm);
}

.app-topbar__copy,
.app-topbar__controls,
.app-topbar__locale,
.app-topbar__status {
  display: grid;
  gap: 8px;
}

.app-topbar__section {
  margin: 0;
  font-size: 0.96rem;
  font-weight: 700;
  color: var(--rr-color-text-primary);
}

.app-topbar__controls {
  grid-auto-flow: column;
  align-items: center;
  justify-content: end;
  gap: var(--rr-space-4);
}

.app-topbar__label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

@media (width <= 900px) {
  .app-topbar {
    flex-direction: column;
    align-items: stretch;
  }

  .app-topbar__controls {
    grid-auto-flow: row;
    justify-content: stretch;
  }
}
</style>
