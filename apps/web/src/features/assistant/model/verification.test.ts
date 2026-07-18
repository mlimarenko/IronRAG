import { describe, expect, it } from 'vitest'
import type { EvidenceBundle } from '@/shared/types'
import { mapAssistantVerificationState, shouldShowVerifiedEvidence } from './verification'

function evidence(
  answerDisposition: EvidenceBundle['answerDisposition'],
  verificationState: EvidenceBundle['verificationState'],
): EvidenceBundle {
  return {
    segmentRefs: [],
    factRefs: [],
    entityRefs: [],
    relationRefs: [],
    verificationState,
    verificationWarnings: [],
    answerDisposition,
    clarification: { required: false, question: null, answerCandidates: [] },
  }
}

describe('mapAssistantVerificationState', () => {
  it('keeps not_run as a neutral state instead of coercing it to failed', () => {
    expect(mapAssistantVerificationState('not_run')).toBe('not_run')
  })

  it('preserves canonical verification states', () => {
    expect(mapAssistantVerificationState('verified')).toBe('passed')
    expect(mapAssistantVerificationState('insufficient_evidence')).toBe('insufficient_evidence')
    expect(mapAssistantVerificationState('failed')).toBe('failed')
  })
})

describe('shouldShowVerifiedEvidence', () => {
  it('shows a verifier verdict only for a factual-ready answer', () => {
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'passed'))).toBe(true)
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'partially_supported'))).toBe(true)
  })

  it('never presents a non-terminal or non-factual answer as verified', () => {
    for (const disposition of ['non_terminal', 'safe_fallback', 'clarification'] as const) {
      expect(shouldShowVerifiedEvidence(evidence(disposition, 'passed'))).toBe(false)
    }
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'not_run'))).toBe(false)
  })
})
