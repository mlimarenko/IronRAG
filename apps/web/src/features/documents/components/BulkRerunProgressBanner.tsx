import { memo, type ReactNode } from 'react'
import type { TFunction } from 'i18next'
import { CheckSquare, Loader2, XCircle } from 'lucide-react'
import { Button } from '@/shared/components/ui/button'
import { ASYNC_OPERATION_TERMINAL_STATES, type AsyncOperationDetail } from '@/shared/api'

export interface BulkRerunProgressState {
  kind: 'delete' | 'reprocess'
  operationId: string
  total: number
  completed: number
  failed: number
  inFlight: number
  status: AsyncOperationDetail['status']
}

type BulkRerunProgressBannerProps = Readonly<{
  bulkRerun: BulkRerunProgressState
  onDismiss: () => void
  t: TFunction
}>

type ProgressPresentation = Readonly<{
  icon: ReactNode
  label: string
  progressClassName: string
  tone: string
}>

function completedLabel(
  bulkRerun: BulkRerunProgressState,
  hasFailures: boolean,
  t: TFunction,
): string {
  if (hasFailures) {
    const key =
      bulkRerun.kind === 'delete'
        ? 'documents.bulkDeleteDoneWithFailures'
        : 'documents.bulkRerunDoneWithFailures'
    return t(key, {
      completed: bulkRerun.completed,
      failed: bulkRerun.failed,
      total: bulkRerun.total,
    })
  }

  const key = bulkRerun.kind === 'delete' ? 'documents.bulkDeleteDone' : 'documents.bulkRerunDone'
  return t(key, { total: bulkRerun.total })
}

function activeLabel(
  bulkRerun: BulkRerunProgressState,
  isFinalizing: boolean,
  settled: number,
  t: TFunction,
): string {
  if (isFinalizing) {
    const key =
      bulkRerun.kind === 'delete'
        ? 'documents.bulkDeleteFinalizing'
        : 'documents.bulkRerunFinalizing'
    return t(key, { total: bulkRerun.total })
  }

  const key =
    bulkRerun.kind === 'delete' ? 'documents.bulkDeleteInFlight' : 'documents.bulkRerunInFlight'
  return t(key, { settled, total: bulkRerun.total })
}

function progressPresentation(
  bulkRerun: BulkRerunProgressState,
  isTerminal: boolean,
  isFinalizing: boolean,
  hasFailures: boolean,
  settled: number,
  t: TFunction,
): ProgressPresentation {
  if (!isTerminal) {
    return {
      icon: <Loader2 className="h-4 w-4 shrink-0 animate-spin text-primary" />,
      label: activeLabel(bulkRerun, isFinalizing, settled, t),
      progressClassName: 'h-full bg-primary transition-all duration-300',
      tone: 'border-primary/30 bg-primary/5',
    }
  }

  if (hasFailures) {
    return {
      icon: <XCircle className="h-4 w-4 shrink-0 text-status-warning" />,
      label: completedLabel(bulkRerun, true, t),
      progressClassName: 'h-full bg-status-warning transition-all duration-300',
      tone: 'border-status-warning/30 bg-status-warning/5',
    }
  }

  return {
    icon: <CheckSquare className="h-4 w-4 shrink-0 text-status-ready" />,
    label: completedLabel(bulkRerun, false, t),
    progressClassName: 'h-full bg-status-ready transition-all duration-300',
    tone: 'border-status-ready/30 bg-status-ready/5',
  }
}

/**
 * Canonical inline progress strip for async batch document operations. Occupies
 * one row above the documents table, never a modal. Surfaces three numbers
 * (completed / total / failed) plus a slim progress bar, and becomes
 * dismissible the moment the parent async-op enters a terminal state.
 */
function BulkRerunProgressBannerImpl({ bulkRerun, onDismiss, t }: BulkRerunProgressBannerProps) {
  const denominator = Math.max(bulkRerun.total, 1)
  const settled = bulkRerun.completed + bulkRerun.failed
  const pct = Math.min(100, Math.round((settled / denominator) * 100))
  const isTerminal = ASYNC_OPERATION_TERMINAL_STATES.has(bulkRerun.status)
  const isFinalizing = !isTerminal && bulkRerun.total > 0 && settled >= bulkRerun.total
  const hasFailures = bulkRerun.failed > 0
  const presentation = progressPresentation(
    bulkRerun,
    isTerminal,
    isFinalizing,
    hasFailures,
    settled,
    t,
  )

  return (
    <div
      className={`flex items-center gap-3 rounded-xl border px-3 py-2 ${presentation.tone}`}
      role="status"
      aria-live="polite"
    >
      {presentation.icon}
      <div className="min-w-0 flex-1">
        <div className="truncate text-xs font-medium">{presentation.label}</div>
        <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-muted">
          <div className={presentation.progressClassName} style={{ width: `${pct}%` }} />
        </div>
      </div>
      {isTerminal && (
        <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={onDismiss}>
          {t('documents.bulkRerunDismiss')}
        </Button>
      )}
    </div>
  )
}

export const BulkRerunProgressBanner = memo(BulkRerunProgressBannerImpl)
