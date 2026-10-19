import type { AiBindingPurpose, ModelPricing } from '@/shared/api/generated'
import type { AdminModelOption } from '../model/aiAdapter'

export type ModelForm = {
  providerCatalogId: string
  modelName: string
  lifecycleState: string
  contextWindow: string
  maxOutputTokens: string
  purposes: AiBindingPurpose[]
  currency: string
  input: string
  cachedInput: string
  cacheWrite: string
  output: string
  longEnabled: boolean
  longThreshold: string
  longInput: string
  longCachedInput: string
  longOutput: string
}

export const EMPTY_MODEL_FORM: ModelForm = {
  providerCatalogId: '',
  modelName: '',
  lifecycleState: 'active',
  contextWindow: '',
  maxOutputTokens: '',
  purposes: [],
  currency: 'USD',
  input: '0',
  cachedInput: '0',
  cacheWrite: '0',
  output: '0',
  longEnabled: false,
  longThreshold: '',
  longInput: '',
  longCachedInput: '',
  longOutput: '',
}

export function formFromModel(model: AdminModelOption): ModelForm {
  const long = model.pricing.longContext
  return {
    providerCatalogId: model.providerCatalogId,
    modelName: model.modelName,
    lifecycleState: model.lifecycleState ?? 'active',
    contextWindow: model.contextWindow == null ? '' : String(model.contextWindow),
    maxOutputTokens: model.maxOutputTokens == null ? '' : String(model.maxOutputTokens),
    purposes: model.allowedBindingPurposes,
    currency: model.pricing.currencyCode,
    input: model.pricing.inputPricePer1m,
    cachedInput: model.pricing.cachedInputPricePer1m,
    cacheWrite: model.pricing.cacheWriteInputPricePer1m,
    output: model.pricing.outputPricePer1m,
    longEnabled: Boolean(long),
    longThreshold: long ? String(long.thresholdTokens) : '',
    longInput: long?.inputPricePer1m ?? '',
    longCachedInput: long?.cachedInputPricePer1m ?? '',
    longOutput: long?.outputPricePer1m ?? '',
  }
}

export function pricingFromForm(form: ModelForm): ModelPricing {
  return {
    currencyCode: form.currency.trim().toUpperCase(),
    inputPricePer1m: form.input,
    cachedInputPricePer1m: form.cachedInput,
    cacheWriteInputPricePer1m: form.cacheWrite,
    outputPricePer1m: form.output,
    ...(form.longEnabled
      ? {
          longContext: {
            thresholdTokens: Number(form.longThreshold),
            inputPricePer1m: form.longInput,
            cachedInputPricePer1m: form.longCachedInput,
            outputPricePer1m: form.longOutput,
          },
        }
      : {}),
  }
}
