import type { EvidenceBundle } from '@/shared/types'

export function shouldShowVerifiedEvidence(evidence: EvidenceBundle): boolean {
  return evidence.answerDisposition === 'factual_ready' && evidence.verificationState !== 'not_run'
}
