import { defaultLocale, supportedLocales, type AppLocale } from './messages'

const localeStorageKey = 'rustrag.locale'
const intlLocaleByAppLocale: Record<AppLocale, string> = {
  en: 'en-US',
  ru: 'ru-RU',
}

export function isSupportedLocale(value: string): value is AppLocale {
  return supportedLocales.includes(value as AppLocale)
}

export function resolveInitialLocale(): AppLocale {
  if (typeof window === 'undefined') {
    return defaultLocale
  }

  const storedLocale = window.localStorage.getItem(localeStorageKey)
  if (storedLocale && isSupportedLocale(storedLocale)) {
    return storedLocale
  }

  const browserLocale = window.navigator.language.split('-')[0]?.toLowerCase()
  if (browserLocale && isSupportedLocale(browserLocale)) {
    return browserLocale
  }

  return defaultLocale
}

export function persistLocale(locale: AppLocale) {
  if (typeof window === 'undefined') {
    return
  }

  window.localStorage.setItem(localeStorageKey, locale)
}

export function getIntlLocale(locale: AppLocale): string {
  return intlLocaleByAppLocale[locale]
}

export function syncDocumentLocale(locale: AppLocale) {
  if (typeof document === 'undefined') {
    return
  }

  document.documentElement.lang = locale
}
