import type { EvidenceBundle, VerificationState } from '@/shared/types'

export function mapAssistantVerificationState(apiState: string): VerificationState {
  const map: Record<string, VerificationState> = {
    verified: 'passed',
    partially_supported: 'partially_supported',
    conflicting: 'conflicting',
    insufficient_evidence: 'insufficient_evidence',
    failed: 'failed',
    not_run: 'not_run',
  }
  return map[apiState] ?? 'failed'
}

export function shouldShowVerifiedEvidence(evidence: EvidenceBundle): boolean {
  return evidence.answerDisposition === 'factual_ready' && evidence.verificationState !== 'not_run'
}
