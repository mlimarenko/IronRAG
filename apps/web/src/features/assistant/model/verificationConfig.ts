import { AlertTriangle, Brain, CheckCircle2, XCircle } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import type { TFunction } from 'i18next';
import type { VerificationState } from '@/shared/types';

type VerificationBadgeConfig = {
  icon: LucideIcon;
  cls: string;
};

/**
 * Static lookup table for verification badge styling. Lives outside the
 * page component so that React does not rebuild it every render and so
 * both the inline-in-thread badge and the evidence panel pull from the
 * same canonical source of truth.
 */
export const VERIFICATION_CONFIG: Record<VerificationState, VerificationBadgeConfig> = {
  passed: {
    icon: CheckCircle2,
    cls: 'text-status-ready',
  },
  partially_supported: {
    icon: AlertTriangle,
    cls: 'text-status-warning',
  },
  conflicting: {
    icon: XCircle,
    cls: 'text-status-failed',
  },
  insufficient_evidence: {
    icon: AlertTriangle,
    cls: 'text-status-sparse',
  },
  failed: {
    icon: XCircle,
    cls: 'text-status-failed',
  },
  not_run: {
    icon: Brain,
    cls: 'text-muted-foreground',
  },
};

export function verificationLabel(state: VerificationState, t: TFunction): string {
  switch (state) {
    case 'passed':
      return t('assistant.verified');
    case 'partially_supported':
      return t('assistant.partiallySupported');
    case 'conflicting':
      return t('assistant.conflictingEvidence');
    case 'insufficient_evidence':
      return t('assistant.insufficientEvidence');
    case 'failed':
      return t('assistant.verificationFailed');
    case 'not_run':
      return t('assistant.verificationNotRun');
  }
}
