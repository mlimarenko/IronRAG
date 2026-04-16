import { memo } from 'react';
import { CheckSquare, Loader2, XCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { ASYNC_OPERATION_TERMINAL_STATES, type AsyncOperationDetail } from '@/api';

export interface BulkRerunProgressState {
  operationId: string;
  total: number;
  completed: number;
  failed: number;
  inFlight: number;
  status: AsyncOperationDetail['status'];
}

type BulkRerunProgressBannerProps = {
  bulkRerun: BulkRerunProgressState;
  onDismiss: () => void;
  t: (key: string, values?: Record<string, unknown>) => string;
};

/**
 * Canonical inline progress strip for the async batch-rerun flow. Occupies
 * one row above the documents table, never a modal. Surfaces three numbers
 * (completed / total / failed) plus a slim progress bar, and becomes
 * dismissible the moment the parent async-op enters a terminal state.
 *
 * Extracted from DocumentsPage so the list page does not re-render this
 * banner on unrelated list/selection state flips — React.memo short-
 * circuits on unchanged `bulkRerun` payload references.
 */
function BulkRerunProgressBannerImpl({
  bulkRerun,
  onDismiss,
  t,
}: BulkRerunProgressBannerProps): JSX.Element {
  const denominator = Math.max(bulkRerun.total, 1);
  const settled = bulkRerun.completed + bulkRerun.failed;
  const pct = Math.min(100, Math.round((settled / denominator) * 100));
  const isTerminal = ASYNC_OPERATION_TERMINAL_STATES.has(bulkRerun.status);
  const hasFailures = bulkRerun.failed > 0;
  const tone = isTerminal
    ? hasFailures
      ? 'border-amber-500/30 bg-amber-50/40 dark:bg-amber-500/5'
      : 'border-emerald-500/30 bg-emerald-50/40 dark:bg-emerald-500/5'
    : 'border-primary/30 bg-primary/5';
  const label = isTerminal
    ? hasFailures
      ? t('documents.bulkRerunDoneWithFailures', {
          completed: bulkRerun.completed,
          failed: bulkRerun.failed,
          total: bulkRerun.total,
        })
      : t('documents.bulkRerunDone', { total: bulkRerun.total })
    : t('documents.bulkRerunInFlight', {
        settled,
        total: bulkRerun.total,
      });

  return (
    <div
      className={`flex items-center gap-3 rounded-xl border px-3 py-2 ${tone}`}
      role="status"
      aria-live="polite"
    >
      {!isTerminal ? (
        <Loader2 className="h-4 w-4 shrink-0 animate-spin text-primary" />
      ) : hasFailures ? (
        <XCircle className="h-4 w-4 shrink-0 text-amber-600" />
      ) : (
        <CheckSquare className="h-4 w-4 shrink-0 text-emerald-600" />
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate text-xs font-medium">{label}</div>
        <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className={
              hasFailures && isTerminal
                ? 'h-full bg-amber-500 transition-all duration-300'
                : isTerminal
                  ? 'h-full bg-emerald-500 transition-all duration-300'
                  : 'h-full bg-primary transition-all duration-300'
            }
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>
      {isTerminal && (
        <Button
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={onDismiss}
        >
          {t('documents.bulkRerunDismiss')}
        </Button>
      )}
    </div>
  );
}

export const BulkRerunProgressBanner = memo(BulkRerunProgressBannerImpl);
