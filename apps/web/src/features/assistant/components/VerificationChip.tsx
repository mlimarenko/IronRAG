import { memo, useState } from 'react'
import type { TFunction } from 'i18next'
import { ChevronDown } from 'lucide-react'
import type { VerificationState } from '@/shared/types'
import { cn } from '@/shared/lib/utils'
import { VERIFICATION_CONFIG, verificationLabel } from '../model/verificationConfig'

type VerificationTone = 'ready' | 'warning' | 'failed' | 'sparse' | 'muted'

const TONE_BY_STATE: Record<VerificationState, VerificationTone> = {
  passed: 'ready',
  partially_supported: 'warning',
  conflicting: 'failed',
  insufficient_evidence: 'sparse',
  failed: 'failed',
  not_run: 'muted',
}

/**
 * Token-driven chip surface. Each tone maps to a status colour family from the
 * 0.5.0 design tokens (`--status-*`), so the verdict reads with semantic colour
 * without hardcoding hex values.
 */
const TONE_STYLE: Record<VerificationTone, { wrap: string; ring: string; icon: string }> = {
  ready: {
    wrap: 'bg-status-ready-bg text-status-ready',
    ring: 'hsl(var(--status-ready-ring) / 0.55)',
    icon: 'text-status-ready',
  },
  warning: {
    wrap: 'bg-status-warning-bg text-status-warning',
    ring: 'hsl(var(--status-warning-ring) / 0.6)',
    icon: 'text-status-warning',
  },
  failed: {
    wrap: 'bg-status-failed-bg text-status-failed',
    ring: 'hsl(var(--status-failed-ring) / 0.6)',
    icon: 'text-status-failed',
  },
  sparse: {
    wrap: 'bg-status-sparse-bg text-status-sparse',
    ring: 'hsl(var(--status-sparse-ring) / 0.6)',
    icon: 'text-status-sparse',
  },
  muted: {
    wrap: 'bg-surface-sunken text-muted-foreground',
    ring: 'hsl(var(--border) / 0.7)',
    icon: 'text-muted-foreground',
  },
}

type VerificationChipProps = {
  t: TFunction
  state: VerificationState
  warnings?: string[]
  className?: string
}

function VerificationChipImpl({
  t,
  state,
  warnings = [],
  className,
}: Readonly<VerificationChipProps>) {
  const [expanded, setExpanded] = useState(false)
  const config = VERIFICATION_CONFIG[state]
  const tone = TONE_STYLE[TONE_BY_STATE[state]]
  const Icon = config.icon
  const label = verificationLabel(state, t)
  const hasWarnings = state !== 'passed' && state !== 'not_run' && warnings.length > 0
  const warningEntries = warnings.map((warning, index) => ({
    key: `${warning}-${warnings.slice(0, index).filter((item) => item === warning).length}`,
    warning,
  }))

  return (
    <div className={cn('flex flex-col gap-1.5', className)}>
      <button
        type="button"
        aria-label={t('assistant.verdictLabel', { verdict: label })}
        aria-expanded={hasWarnings ? expanded : undefined}
        disabled={!hasWarnings}
        onClick={hasWarnings ? () => setExpanded((v) => !v) : undefined}
        className={cn(
          'inline-flex w-fit items-center gap-2 rounded-full px-3 py-1.5 text-xs font-bold tracking-tight transition-all duration-200',
          tone.wrap,
          hasWarnings
            ? 'cursor-pointer hover:brightness-[0.97] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2'
            : 'cursor-default',
        )}
        style={{ boxShadow: `inset 0 0 0 1px ${tone.ring}` }}
      >
        <Icon className={cn('h-3.5 w-3.5 shrink-0', tone.icon)} aria-hidden="true" />
        <span>{label}</span>
        {hasWarnings && (
          <ChevronDown
            className={cn(
              'h-3.5 w-3.5 shrink-0 transition-transform duration-200',
              expanded && 'rotate-180',
            )}
            aria-hidden="true"
          />
        )}
      </button>
      {hasWarnings && expanded && (
        <ul className="animate-fade-in space-y-1 workbench-surface border-border/60 px-3 py-2.5 text-xs leading-relaxed text-muted-foreground">
          {warningEntries.map(({ key, warning }) => (
            <li key={key} className="flex gap-1.5">
              <span
                aria-hidden="true"
                className={cn('mt-1.5 h-1 w-1 shrink-0 rounded-full', tone.icon)}
              />
              <span className="min-w-0 break-words">{warning}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

export const VerificationChip = memo(VerificationChipImpl)
