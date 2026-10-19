import type { TFunction } from 'i18next'
import type { VerificationState } from '@/shared/types'

export const VERIFICATION_TONE: Record<VerificationState, string> = {
  verified: 'verified',
  partially_supported: 'partially_supported',
  conflicting: 'conflicting',
  insufficient_evidence: 'sparse',
  failed: 'failed',
  not_run: 'neutral',
}

export function verificationLabel(state: VerificationState, t: TFunction): string {
  switch (state) {
    case 'verified':
      return t('assistant.verified')
    case 'partially_supported':
      return t('assistant.partiallySupported')
    case 'conflicting':
      return t('assistant.conflictingEvidence')
    case 'insufficient_evidence':
      return t('assistant.insufficientEvidence')
    case 'failed':
      return t('assistant.verificationFailed')
    case 'not_run':
      return t('assistant.verificationNotRun')
  }
}
