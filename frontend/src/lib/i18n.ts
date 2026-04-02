import { createI18n } from 'vue-i18n'
import en from 'src/i18n/en'
import ru from 'src/i18n/ru'

const LOCALE_STORAGE_KEY = 'rustrag.ui.locale'

function resolveInitialLocale(): 'en' | 'ru' {
  if (typeof window === 'undefined') {
    return 'ru'
  }

  const stored = window.localStorage.getItem(LOCALE_STORAGE_KEY)
  return stored === 'en' || stored === 'ru' ? stored : 'ru'
}

export const i18n = createI18n({
  legacy: false,
  locale: resolveInitialLocale(),
  fallbackLocale: 'en',
  messages: {
    en,
    ru,
  },
})
