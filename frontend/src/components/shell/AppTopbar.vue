<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import { useAppLocale } from 'src/composables/useAppLocale'

const { t } = useI18n()
const { locale, localeOptions, setLocale } = useAppLocale()
const applyLocale = (nextLocale: (typeof localeOptions)[number]) => {
  setLocale(nextLocale)
}
const localeButtons = computed(() => {
  void locale.value

  return localeOptions.map((option) => ({
    value: option,
    code: t(`shell.locale.${option}`),
    name: t(`common.localeName.${option}`),
  }))
})

withDefaults(
  defineProps<{
    sectionLabel?: string
    sectionSummary?: string
    sectionKey?: 'home' | 'processing' | 'files' | 'search' | 'graph' | 'api'
  }>(),
  {
    sectionLabel: undefined,
    sectionSummary: undefined,
    sectionKey: undefined,
  },
)
</script>

<template>
  <header class="app-topbar">
    <div class="app-topbar__copy">
      <p class="app-topbar__label">{{ t('shell.topbar.surface') }}</p>
      <p class="app-topbar__section">{{ sectionLabel }}</p>
      <p v-if="sectionSummary" class="app-topbar__summary">
        {{ sectionSummary }}
      </p>
    </div>

    <div class="app-topbar__locale" role="group" :aria-label="t('shell.topbar.language')">
      <span class="app-topbar__locale-label">{{ t('shell.topbar.languageHint') }}</span>
      <div class="app-topbar__locale-switch">
        <button
          v-for="option in localeButtons"
          :key="option.value"
          type="button"
          class="app-topbar__locale-button"
          :data-active="locale === option.value"
          :aria-pressed="locale === option.value"
          :aria-label="option.name"
          :title="option.name"
          @click="applyLocale(option.value)"
        >
          <span class="app-topbar__locale-code">{{ option.code }}</span>
          <span class="app-topbar__locale-name">{{ option.name }}</span>
        </button>
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

.app-topbar__copy {
  display: grid;
  gap: 2px;
  min-width: 0;
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

.app-topbar__summary,
.app-topbar__workflow {
  margin: 0;
  font-size: 0.88rem;
  color: var(--rr-color-text-secondary);
}

.app-topbar__workflow {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
}

.app-topbar__locale {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  padding: 6px;
  border: 1px solid rgb(15 23 42 / 0.1);
  border-radius: 999px;
  background: rgb(255 255 255 / 0.78);
  box-shadow: 0 8px 24px rgb(15 23 42 / 0.04);
}

.app-topbar__locale-label {
  padding-left: 8px;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.07em;
  text-transform: uppercase;
  color: var(--rr-color-text-muted);
  white-space: nowrap;
}

.app-topbar__locale-switch {
  display: inline-flex;
  gap: 4px;
  padding: 4px;
  border-radius: 999px;
  background: rgb(248 250 252 / 0.92);
}

.app-topbar__locale-button {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-height: 36px;
  padding: 0 12px;
  border: 0;
  border-radius: 999px;
  background: transparent;
  color: var(--rr-color-text-secondary);
  cursor: pointer;
  transition:
    background var(--rr-motion-base),
    color var(--rr-motion-base),
    box-shadow var(--rr-motion-base),
    transform var(--rr-motion-base);
}

.app-topbar__locale-button:hover {
  color: var(--rr-color-text-primary);
}

.app-topbar__locale-button[data-active='true'] {
  background: var(--rr-color-bg-contrast);
  color: var(--rr-color-text-inverse);
  box-shadow: 0 8px 20px rgb(15 23 42 / 0.16);
}

.app-topbar__locale-button:focus-visible {
  outline: 2px solid rgb(59 130 246 / 0.55);
  outline-offset: 2px;
}

.app-topbar__locale-code {
  font-size: 0.78rem;
  font-weight: 800;
  letter-spacing: 0.08em;
}

.app-topbar__locale-name {
  font-size: 0.85rem;
  font-weight: 650;
}

@media (width <= 900px) {
  .app-topbar {
    align-items: flex-start;
    flex-direction: column;
    padding-top: 0;
  }

  .app-topbar__locale {
    width: 100%;
    justify-content: space-between;
  }
}

@media (width <= 640px) {
  .app-topbar__locale {
    gap: 6px;
    align-items: stretch;
    flex-direction: column;
    border-radius: calc(var(--rr-radius-md) + 4px);
  }

  .app-topbar__locale-label {
    padding-left: 6px;
  }

  .app-topbar__locale-button {
    justify-content: center;
    flex: 1 1 0;
  }

  .app-topbar__locale-name {
    display: none;
  }
}
</style>
