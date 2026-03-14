import type { App as VueApp } from 'vue'
import { createI18n } from 'vue-i18n'

import { messages, defaultLocale } from 'src/i18n/messages'
import { resolveInitialLocale } from 'src/i18n/runtime'

export const i18n = createI18n({
  legacy: false,
  locale: resolveInitialLocale(),
  fallbackLocale: defaultLocale,
  messages,
})

export default ({ app }: { app: VueApp }) => {
  app.use(i18n)
}
