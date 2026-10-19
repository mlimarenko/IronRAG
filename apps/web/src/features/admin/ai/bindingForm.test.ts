import { describe, expect, it } from 'vitest'

import {
  bindingRequestTuning,
  applyManagedReasoningMode,
  deriveReasoningJournal,
  hasManagedReasoningMode,
  parseStoredReasoningMode,
  rawReasoningConflict,
  requestTuningValue,
  withRequestTuning,
  withoutManagedReasoningMode,
  EMPTY_BINDING_FORM,
} from './bindingForm'

describe('binding request tuning', () => {
  it('reads and writes one key of the additional-parameters JSON', () => {
    const json = withRequestTuning('{"seed": 7}', 'serviceTier', 'flex')
    expect(JSON.parse(json)).toEqual({ seed: 7, service_tier: 'flex' })
    expect(requestTuningValue(json, 'serviceTier')).toBe('flex')
    expect(requestTuningValue(json, 'reasoningEffort')).toBe('')
  })

  it('removes the key for the provider default and empties an empty object', () => {
    const json = withRequestTuning('{"reasoning_effort": "none"}', 'reasoningEffort', '')
    expect(json).toBe('')
  })

  it('leaves invalid JSON untouched and reports it', () => {
    expect(withRequestTuning('{oops', 'serviceTier', 'flex')).toBe('{oops')
    expect(requestTuningValue('{oops', 'serviceTier')).toBeNull()
  })

  it('shows only the owned keys on a binding card', () => {
    expect(
      bindingRequestTuning({ service_tier: 'flex', reasoning_effort: 'none', seed: 7 }),
    ).toEqual({ serviceTier: 'flex', reasoningEffort: 'none' })
    expect(bindingRequestTuning(undefined)).toEqual({})
  })
})

describe('managed reasoning mode', () => {
  it('keeps the managed key out of the raw editor and reports what it held', () => {
    const { extraJson, stored } = withoutManagedReasoningMode(
      '{"seed": 7, "reasoningMode": {"version": 1, "mode": "none"}}',
    )
    expect(JSON.parse(extraJson)).toEqual({ seed: 7 })
    expect(stored).toEqual({ kind: 'selected', mode: 'none' })
  })

  it('reports an unreadable stored form instead of treating it as unset', () => {
    expect(parseStoredReasoningMode({ version: 2, mode: 'none' })).toEqual({ kind: 'invalid' })
    expect(parseStoredReasoningMode({ version: 1, mode: 'turbo' })).toEqual({ kind: 'invalid' })
    expect(parseStoredReasoningMode('none')).toEqual({ kind: 'invalid' })
    expect(parseStoredReasoningMode({ version: 1, mode: 'none', sneaky: true })).toEqual({
      kind: 'invalid',
    })
    expect(parseStoredReasoningMode(undefined)).toEqual({ kind: 'unset' })
  })

  it('merges the typed selection and clears it for unset without dropping foreign keys', () => {
    expect(applyManagedReasoningMode({ seed: 7 }, 'none')).toEqual({
      seed: 7,
      reasoningMode: { version: 1, mode: 'none' },
    })
    expect(
      applyManagedReasoningMode({ seed: 7, reasoningMode: { version: 1, mode: 'none' } }, ''),
    ).toEqual({ seed: 7 })
    expect(hasManagedReasoningMode('{"reasoningMode": {"version": 1, "mode": "none"}}')).toBe(true)
    expect(hasManagedReasoningMode('{"seed": 7}')).toBe(false)
  })
})

describe('raw reasoning conflict', () => {
  it('permits only the exact declared wire value', () => {
    expect(rawReasoningConflict('reasoning_effort_none', '{"reasoning_effort": "none"}')).toBeNull()
    expect(rawReasoningConflict('reasoning_effort_none', '{"reasoning_effort": "high"}')).toEqual({
      parameter: 'reasoning_effort',
      kind: 'disagreeing_value',
    })
    expect(
      rawReasoningConflict('thinking_disabled', '{"thinking": {"type": "disabled"}}'),
    ).toBeNull()
    expect(rawReasoningConflict('thinking_disabled', '{"thinking": {"type": "enabled"}}')).toEqual({
      parameter: 'thinking',
      kind: 'disagreeing_value',
    })
  })

  it('rejects the other owned switch even when it also disables reasoning', () => {
    expect(
      rawReasoningConflict('reasoning_effort_none', '{"thinking": {"type": "disabled"}}'),
    ).toEqual({ parameter: 'thinking', kind: 'other_control' })
    expect(rawReasoningConflict('thinking_disabled', '{"reasoning_effort": "none"}')).toEqual({
      parameter: 'reasoning_effort',
      kind: 'other_control',
    })
  })
})

describe('reasoning journal', () => {
  const base = {
    purpose: 'query_answer' as const,
    hasSelectedAccount: true,
    capability: {
      state: 'confirmed',
      mode: 'none',
      wire: 'reasoning_effort_none',
      evidence: 'fixture',
    } as const,
    runtimeCapable: true,
  }

  it('offers the confirmed mode and blocks nothing on an untouched form', () => {
    const journal = deriveReasoningJournal({ ...base, form: EMPTY_BINDING_FORM })
    expect(journal.confirmed).toBe(true)
    expect(journal.modes).toEqual(['none'])
    expect(journal.show).toBe(true)
    expect(journal.blocked).toBe(false)
  })

  it('keeps the field hidden on embed_chunk until the typed key needs a decision', () => {
    const journal = deriveReasoningJournal({
      ...base,
      purpose: 'embed_chunk',
      form: EMPTY_BINDING_FORM,
    })
    expect(journal.show).toBe(false)
  })

  it('shows and blocks on an unreadable stored selection until Unset', () => {
    const journal = deriveReasoningJournal({
      ...base,
      form: { ...EMPTY_BINDING_FORM, reasoningStored: { kind: 'invalid' } as const },
    })
    expect(journal.show).toBe(true)
    expect(journal.storedInvalid).toBe(true)
    expect(journal.blocked).toBe(true)
  })

  it('blocks a pending mode the current pair does not confirm', () => {
    const journal = deriveReasoningJournal({
      ...base,
      capability: { state: 'unconfirmed' },
      form: { ...EMPTY_BINDING_FORM, reasoningMode: 'none' as const },
    })
    expect(journal.confirmed).toBe(false)
    expect(journal.modeUnsupported).toBe(true)
    expect(journal.blocked).toBe(true)
    expect(journal.readonlyKey).toBe('unconfirmed')
  })

  it('blocks a carried selection after the pair changed', () => {
    const journal = deriveReasoningJournal({
      ...base,
      form: {
        ...EMPTY_BINDING_FORM,
        accountId: 'account-a',
        modelId: 'model-b',
        reasoningMode: 'none' as const,
        reasoningTarget: { accountId: 'account-a', modelId: 'model-a' },
      },
    })
    expect(journal.targetChanged).toBe(true)
    expect(journal.blocked).toBe(true)
  })

  it('blocks a raw conflict only while a mode is pending', () => {
    const form = {
      ...EMPTY_BINDING_FORM,
      reasoningMode: 'none' as const,
      extraJson: '{"reasoning_effort": "high"}',
    }
    const journal = deriveReasoningJournal({ ...base, form })
    expect(journal.rawConflict).toEqual({
      parameter: 'reasoning_effort',
      kind: 'disagreeing_value',
    })
    expect(journal.blocked).toBe(true)
    const unset = deriveReasoningJournal({ ...base, form: { ...form, reasoningMode: '' as const } })
    expect(unset.rawConflict).toBeNull()
    expect(unset.blocked).toBe(false)
  })

  it('names the read-only explanation for each unconfirmed capability', () => {
    const invalid = deriveReasoningJournal({
      ...base,
      capability: { state: 'invalid' },
      form: EMPTY_BINDING_FORM,
    })
    expect(invalid.readonlyKey).toBe('invalidCapability')
    const foreignRuntime = deriveReasoningJournal({
      ...base,
      runtimeCapable: false,
      form: EMPTY_BINDING_FORM,
    })
    expect(foreignRuntime.confirmed).toBe(false)
    expect(foreignRuntime.readonlyKey).toBe('runtimeUnsupported')
  })

  it('blocks while the raw editor carries the managed key', () => {
    const journal = deriveReasoningJournal({
      ...base,
      form: {
        ...EMPTY_BINDING_FORM,
        extraJson: '{"reasoningMode": {"version": 1, "mode": "none"}}',
      },
    })
    expect(journal.managedKeyTyped).toBe(true)
    expect(journal.blocked).toBe(true)
  })
})
