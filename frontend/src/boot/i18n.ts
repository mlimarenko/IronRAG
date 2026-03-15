import { watch } from 'vue'
import type { App as VueApp } from 'vue'
import { createI18n } from 'vue-i18n'

import { messages, defaultLocale, type AppLocale } from 'src/i18n/messages'
import { resolveInitialLocale, syncDocumentLocale } from 'src/i18n/runtime'

const initialLocale = resolveInitialLocale()

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: defaultLocale,
  messages,
})

watch(
  () => i18n.global.locale.value,
  (locale) => {
    syncDocumentLocale(locale as AppLocale)
  },
  { immediate: true },
)

export default ({ app }: { app: VueApp }) => {
  app.use(i18n)
}
