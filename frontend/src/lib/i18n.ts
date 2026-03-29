import { createI18n } from 'vue-i18n'
import en from 'src/i18n/en'
import ru from 'src/i18n/ru'

export const i18n = createI18n({
  legacy: false,
  locale: 'ru',
  fallbackLocale: 'en',
  messages: {
    en,
    ru,
  },
})
