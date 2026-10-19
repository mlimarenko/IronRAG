import type { TFunction } from 'i18next'

import type { ModelPricing } from '@/shared/api/generated'
import type { AIScopeKind } from '@/shared/types'

export function aiScopeLabel(scope: AIScopeKind, t: TFunction): string {
  return t(`admin.aiPanel.scopeNames.${scope}`)
}

export function formatModelPricing(pricing: ModelPricing, locale: string, t: TFunction): string {
  const input = Number(pricing.inputPricePer1m)
  const output = Number(pricing.outputPricePer1m)
  if ((!Number.isFinite(input) || input === 0) && (!Number.isFinite(output) || output === 0)) {
    return t('admin.aiPanel.pricing.notSet')
  }

  const formatter = new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: pricing.currencyCode.toUpperCase(),
    minimumFractionDigits: 0,
    maximumFractionDigits: 6,
  })
  return t('admin.aiPanel.pricing.summary', {
    input: formatter.format(input || 0),
    output: formatter.format(output || 0),
  })
}
