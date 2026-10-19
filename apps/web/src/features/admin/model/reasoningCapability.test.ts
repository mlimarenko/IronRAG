import { describe, expect, it } from 'vitest'

import {
  parseReasoningCapability,
  selectableReasoningModes,
  type ReasoningCapability,
} from './reasoningCapability'

const confirmed = (overrides: Record<string, unknown> = {}) => ({
  reasoningCapability: {
    confirmed: {
      mode: 'none',
      wire: 'reasoning_effort_none',
      evidence: 'https://developers.openai.com/api/docs/guides/reasoning',
      ...overrides,
    },
  },
})

describe('parseReasoningCapability', () => {
  it('reads an absent pair capability as the honest unconfirmed state', () => {
    expect(parseReasoningCapability({ seedSource: 'clean_v1' })).toEqual({ state: 'unconfirmed' })
    expect(parseReasoningCapability(null)).toEqual({ state: 'unconfirmed' })
    expect(parseReasoningCapability({ reasoningCapability: 'unconfirmed' })).toEqual({
      state: 'unconfirmed',
    })
  })

  it('confirms mode none only with a documented wire and non-blank evidence', () => {
    expect(parseReasoningCapability(confirmed())).toEqual({
      state: 'confirmed',
      mode: 'none',
      wire: 'reasoning_effort_none',
      evidence: 'https://developers.openai.com/api/docs/guides/reasoning',
    })
    expect(
      parseReasoningCapability(confirmed({ wire: 'thinking_disabled', evidence: 'docs' })),
    ).toMatchObject({ state: 'confirmed', wire: 'thinking_disabled' })
  })

  it('fails closed on blank evidence, an unknown wire, an unknown mode, or a versioned form', () => {
    expect(parseReasoningCapability(confirmed({ evidence: '   ' }))).toEqual({ state: 'invalid' })
    expect(parseReasoningCapability(confirmed({ wire: 'guessed_wire' }))).toEqual({
      state: 'invalid',
    })
    expect(parseReasoningCapability(confirmed({ mode: 'high' }))).toEqual({ state: 'invalid' })
    expect(parseReasoningCapability({ reasoningCapability: { confirmed: 'yes' } })).toEqual({
      state: 'invalid',
    })
    expect(parseReasoningCapability({ reasoningCapability: { unconfirmed: true } })).toEqual({
      state: 'invalid',
    })
  })

  it('offers only the confirmed none mode and never a mode from unconfirmed metadata', () => {
    const cap: ReasoningCapability = parseReasoningCapability(confirmed())
    expect(selectableReasoningModes(cap)).toEqual(['none'])
    expect(selectableReasoningModes({ state: 'unconfirmed' })).toEqual([])
    expect(selectableReasoningModes({ state: 'invalid' })).toEqual([])
  })
})
