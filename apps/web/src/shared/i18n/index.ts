import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import en from './en.json'
import ru from './ru.json'

const initialization = i18n.use(initReactI18next).init({
  resources: { en: { translation: en }, ru: { translation: ru } },
  lng: localStorage.getItem('ironrag.v1.locale') || 'en',
  fallbackLng: 'en',
  interpolation: { escapeValue: false },
})

// The bundled resources make initialization synchronous in normal operation.
// Retain the fallback locale if a third-party i18n plugin rejects during startup.
try {
  await initialization
} catch {
  await i18n.changeLanguage('en')
}

export default i18n
