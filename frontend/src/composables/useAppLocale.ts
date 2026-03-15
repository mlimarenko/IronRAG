import { computed } from 'vue'

import { i18n } from 'src/boot/i18n'
import { persistLocale } from 'src/i18n/runtime'
import { supportedLocales, type AppLocale } from 'src/i18n/messages'

export function useAppLocale() {
  const locale = computed({
    get: () => i18n.global.locale.value,
    set: (value: AppLocale) => {
      i18n.global.locale.value = value
      persistLocale(value)
    },
  })

  const setLocale = (value: AppLocale) => {
    locale.value = value
  }

  return {
    locale,
    localeOptions: supportedLocales,
    setLocale,
  }
}
