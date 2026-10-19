import { describe, expect, it } from 'vitest'
import type { EvidenceBundle } from '@/shared/types'
import { shouldShowVerifiedEvidence } from './verification'

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

describe('shouldShowVerifiedEvidence', () => {
  it('shows a verifier verdict only for a factual-ready answer', () => {
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'verified'))).toBe(true)
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'partially_supported'))).toBe(true)
  })

  it('never presents a non-terminal or non-factual answer as verified', () => {
    for (const disposition of ['non_terminal', 'safe_fallback', 'clarification'] as const) {
      expect(shouldShowVerifiedEvidence(evidence(disposition, 'verified'))).toBe(false)
    }
    expect(shouldShowVerifiedEvidence(evidence('factual_ready', 'not_run'))).toBe(false)
  })
})
