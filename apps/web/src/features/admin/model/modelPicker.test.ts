import { describe, expect, it } from 'vitest'

import type { AdminModelOption } from './aiAdapter'
import { buildModelPickerOptions } from './modelPicker'

function catalogModel(overrides: Partial<AdminModelOption> = {}): AdminModelOption {
  return {
    id: 'model-alpha',
    providerCatalogId: 'provider-alpha',
    modelName: 'alpha-chat',
    allowedBindingPurposes: ['query_answer'],
    availabilityState: 'available',
    availableAccountIds: ['account-alpha'],
    reasoningCapability: { state: 'unconfirmed' },
    seeded: true,
    pricing: {
      currencyCode: 'USD',
      inputPricePer1m: '0',
      cachedInputPricePer1m: '0',
      cacheWriteInputPricePer1m: '0',
      outputPricePer1m: '0',
    },
    ...overrides,
  }
}

describe('buildModelPickerOptions', () => {
  it('offers what the provider has right now, not only what the catalog was seeded with', () => {
    // The router case that started this: no seeded chat model, so the dropdown
    // was empty and said nothing.
    const options = buildModelPickerOptions({
      purpose: 'query_answer',
      catalog: [],
      discovered: [{ modelName: 'vendor-x/model-9', inputModalities: [] }],
    })

    expect(options).toHaveLength(1)
    expect(options[0]?.modelName).toBe('vendor-x/model-9')
    expect(options[0]?.selectable).toBe(true)
    expect(options[0]?.note).toBe('not_in_catalog')
    expect(options[0]?.modelCatalogId).toBeUndefined()
  })

  it('keeps a seeded model that cannot serve the purpose visible and disabled', () => {
    const options = buildModelPickerOptions({
      purpose: 'extract_text',
      catalog: [catalogModel()],
      discovered: [],
    })

    expect(options[0]?.selectable).toBe(false)
    expect(options[0]?.note).toBe('cannot_serve')
  })

  it('does not offer a retired catalog row even when its purposes fit', () => {
    // The seed retired the row; the provider may still answer for the id, and
    // without the lifecycle gate the row came back as a selectable option.
    const options = buildModelPickerOptions({
      purpose: 'query_answer',
      catalog: [catalogModel({ lifecycleState: 'deprecated' })],
      discovered: [],
    })

    expect(options[0]?.selectable).toBe(false)
    expect(options[0]?.note).toBe('inactive')
  })

  it('keeps a preview row selectable', () => {
    const options = buildModelPickerOptions({
      purpose: 'query_answer',
      catalog: [catalogModel({ lifecycleState: 'preview' })],
      discovered: [],
    })

    expect(options[0]?.selectable).toBe(true)
    expect(options[0]?.note).toBeUndefined()
  })

  it('lets an operator extend their own row to a second purpose', () => {
    // A model added from the provider's listing carries the purpose it was
    // bound for. Binding it to a second one is an edit, not a contradiction —
    // and blocking it was the dead end the catalog row itself created.
    const options = buildModelPickerOptions({
      purpose: 'agent',
      catalog: [catalogModel({ seeded: false })],
      discovered: [{ modelName: 'alpha-chat', inputModalities: [] }],
    })

    expect(options).toHaveLength(1)
    expect(options[0]?.selectable).toBe(true)
    expect(options[0]?.note).toBe('adds_purpose')
    expect(options[0]?.modelCatalogId).toBe('model-alpha')
  })

  it('blocks document extraction on a model the provider says takes no images', () => {
    const options = buildModelPickerOptions({
      purpose: 'extract_text',
      catalog: [],
      discovered: [{ modelName: 'text-only', inputModalities: ['text'] }],
    })

    expect(options[0]?.selectable).toBe(false)
    expect(options[0]?.note).toBe('needs_vision')
  })

  it('treats an unstated modality as unknown rather than as a refusal', () => {
    // OpenAI's own listing is just an id. Reading that silence as "text only"
    // would block every capable model behind a provider that says less.
    const options = buildModelPickerOptions({
      purpose: 'extract_text',
      catalog: [],
      discovered: [{ modelName: 'says-nothing', inputModalities: [] }],
    })

    expect(options[0]?.selectable).toBe(true)
  })

  it('lists a model once when the catalog and the provider both name it', () => {
    const options = buildModelPickerOptions({
      purpose: 'query_answer',
      catalog: [catalogModel()],
      discovered: [{ modelName: 'alpha-chat', inputModalities: ['text'] }],
    })

    expect(options).toHaveLength(1)
    expect(options[0]?.modelCatalogId).toBe('model-alpha')
    expect(options[0]?.note).toBeUndefined()
  })
})
