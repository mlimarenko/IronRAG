import { AlertTriangle, Brain, CheckCircle2, XCircle } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import type { VerificationState } from '@/types';

export type VerificationBadgeConfig = {
  icon: LucideIcon;
  labelKey: string;
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
    labelKey: 'assistant.verified',
    cls: 'text-status-ready',
  },
  partially_supported: {
    icon: AlertTriangle,
    labelKey: 'assistant.partiallySupported',
    cls: 'text-status-warning',
  },
  conflicting: {
    icon: XCircle,
    labelKey: 'assistant.conflictingEvidence',
    cls: 'text-status-failed',
  },
  insufficient_evidence: {
    icon: AlertTriangle,
    labelKey: 'assistant.insufficientEvidence',
    cls: 'text-status-sparse',
  },
  failed: {
    icon: XCircle,
    labelKey: 'assistant.verificationFailed',
    cls: 'text-status-failed',
  },
  not_run: {
    icon: Brain,
    labelKey: 'assistant.verificationNotRun',
    cls: 'text-muted-foreground',
  },
};
