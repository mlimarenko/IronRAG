import apiEn from './locales/en/api'
import flowEn from './locales/en/flow'
import onboardingEn from './locales/en/onboarding'
import providersEn from './locales/en/providers'
import shellEn from './locales/en/shell'
import apiRu from './locales/ru/api'
import flowRu from './locales/ru/flow'
import onboardingRu from './locales/ru/onboarding'
import providersRu from './locales/ru/providers'
import shellRu from './locales/ru/shell'

export const messages = {
  en: {
    api: apiEn,
    flow: flowEn,
    onboarding: onboardingEn,
    providers: providersEn,
    shell: shellEn,
  },
  ru: {
    api: apiRu,
    flow: flowRu,
    onboarding: onboardingRu,
    providers: providersRu,
    shell: shellRu,
  },
} as const

export type AppLocale = keyof typeof messages

export const defaultLocale: AppLocale = 'en'
export const supportedLocales = Object.keys(messages) as AppLocale[]
