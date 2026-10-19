import {
  ArrowDown,
  ArrowUp,
  CirclePause,
  CirclePlay,
  LoaderCircle,
  RotateCcw,
  Square,
} from 'lucide-react'
import { useCallback, useMemo, useState } from 'react'
import type { SetStateAction } from 'react'
import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'

import { DataGrid, type GridServiceColumn, type GridState } from '@/shared/data-grid'
import { useApp } from '@/shared/contexts/app-context'
import { useQueuedSearchParams } from '@/shared/hooks/useQueuedSearchParams'
import { humanizeDocumentStage } from '@/shared/lib/document-processing'
import { queueStateLabel, stageStatusLabel } from '@/shared/lib/enumLabels'
import { formatRelative } from '@/shared/format'
import { errorMessage } from '@/shared/lib/errorMessage'
import { screenPhase } from '@/shared/lib/screenPhase'
import { Badge, Button, Drawer, PageHeader, Panel } from '@/shared/ui'

import { useIngestQueue, useQueueLibrary, useQueueInspector, useQueueMutations } from './queue/api'
import {
  parseQueueSort,
  queueBucketFor,
  queueBucketKeys,
  queueFiltersFromUrl,
  queueGridColumns,
  QueueResponseError,
  queueScreenPhase,
  resolveQueueRoute,
  serializeQueueFilters,
  type QueueItem,
  type QueueStateFilter,
} from './queue/model'

/**
 * The queue view's error text. A `QueueResponseError` is a wire-contract
 * violation whose raw field diagnostic belongs in operator logs, not in the
 * user's message, so the localized load failure is shown instead. Any other
 * error (transport, HTTP) keeps its own detail.
 */
function queueErrorText(error: unknown, fallback: string): string {
  return error instanceof QueueResponseError ? fallback : errorMessage(error, fallback)
}

const QUEUE_PAGE_SIZE_OPTIONS = [25, 50, 100] as const
const DEFAULT_QUEUE_PAGE_SIZE = 50

function queueGridState(params: URLSearchParams): GridState {
  const size = Number(params.get('pageSize'))
  const page = Number(params.get('page'))
  const { stateFilter } = resolveQueueRoute(params, undefined, undefined)
  return {
    search: params.get('q') ?? '',
    sort: parseQueueSort(params.get('sort')),
    filters: queueFiltersFromUrl(stateFilter, params.get('filters')),
    page: Number.isInteger(page) && page > 0 ? page : 1,
    pageSize: (QUEUE_PAGE_SIZE_OPTIONS as readonly number[]).includes(size)
      ? size
      : DEFAULT_QUEUE_PAGE_SIZE,
  }
}

// The timeline dot only distinguishes three things: finished, in flight, and
// everything else (queued, cancelled, failed) which carries no dot at all.
function stageDotClass(stageState: string): string {
  if (stageState === 'completed') return 'is-done'
  if (stageState === 'running' || stageState === 'started') return 'is-live'
  return ''
}

function QueueInspector({
  item,
  onMove,
  movePending,
}: Readonly<{
  item: QueueItem
  movePending: boolean
  onMove: (direction: 'bottom' | 'down' | 'top' | 'up') => void
}>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const [attemptSelection, setAttemptSelection] = useState<{
    attemptId: string
    jobId: string
  } | null>(null)
  const requestedAttemptId =
    attemptSelection?.jobId === item.jobId ? attemptSelection.attemptId : (item.attemptId ?? null)
  const inspector = useQueueInspector(item.jobId, requestedAttemptId)
  const attempts = inspector.attemptsQuery.data ?? []
  const attemptsPhase = screenPhase({
    loading: inspector.attemptsQuery.isLoading,
    failed: false,
    empty: attempts.length === 0,
  })
  const timelinePhase = screenPhase({
    loading: inspector.stagesQuery.isLoading,
    failed: Boolean(inspector.stagesQuery.error),
    empty: (inspector.stagesQuery.data?.length ?? 0) === 0,
  })

  return (
    <div className="job-inspector">
      <div className="job-heading">
        <Badge tone={item.queueState}>{queueStateLabel(item.queueState, t)}</Badge>
        <code>{item.jobId}</code>
      </div>
      <div className="node-metrics">
        <div>
          <span>{t('common.workspace')}</span>
          <strong>{item.workspaceName}</strong>
        </div>
        <div>
          <span>{t('common.library')}</span>
          <strong>{item.libraryName}</strong>
        </div>
        <div>
          <span>{t('admin.attempt')}</span>
          <strong>
            {inspector.attemptQuery.data?.attempt.attempt_number ?? item.attemptNumber ?? '—'}
          </strong>
        </div>
      </div>

      <section>
        <h3>{t('admin.queueControls')}</h3>
        <div className="node-actions">
          <Button size="sm" disabled={movePending} onClick={() => onMove('top')}>
            <ArrowUp size={14} />
            {t('admin.queueMoveTop')}
          </Button>
          <Button size="sm" disabled={movePending} onClick={() => onMove('up')}>
            <ArrowUp size={14} />
            {t('admin.queueMoveUp')}
          </Button>
          <Button size="sm" disabled={movePending} onClick={() => onMove('down')}>
            <ArrowDown size={14} />
            {t('admin.queueMoveDown')}
          </Button>
          <Button size="sm" disabled={movePending} onClick={() => onMove('bottom')}>
            {t('admin.queueMoveBottom')}
          </Button>
        </div>
      </section>

      <section>
        <h3>{t('admin.queueAttempts')}</h3>
        {attemptsPhase === 'loading' && <small>{t('common.loading')}</small>}
        {attemptsPhase === 'empty' && <small>{t('admin.queueInspectorNoAttempt')}</small>}
        {attemptsPhase === 'ready' && (
          <div className="queue-attempts">
            {attempts.map((attempt) => (
              <button
                className={attempt.id === inspector.resolvedAttemptId ? 'is-active' : ''}
                key={attempt.id}
                onClick={() => setAttemptSelection({ attemptId: attempt.id, jobId: item.jobId })}
                type="button"
              >
                <strong>#{attempt.attempt_number}</strong>
                <span>{queueStateLabel(attempt.attempt_state, t)}</span>
              </button>
            ))}
          </div>
        )}
      </section>

      <section>
        <h3>{t('admin.queueInspectorTimeline')}</h3>
        {timelinePhase === 'loading' && <small>{t('admin.queueInspectorLoading')}</small>}
        {timelinePhase === 'error' && (
          <div className="failure-card" role="alert">
            <strong>{t('admin.queueInspectorError')}</strong>
            <p>{errorMessage(inspector.stagesQuery.error, t('admin.queueInspectorError'))}</p>
          </div>
        )}
        {timelinePhase === 'empty' && <small>{t('admin.queueInspectorNoEvents')}</small>}
        {timelinePhase === 'ready' && (
          <div className="job-timeline">
            {inspector.stagesQuery.data?.map((event) => (
              <div key={event.id}>
                <span className={stageDotClass(event.stage_state)} />
                <div>
                  <strong>{humanizeDocumentStage(event.stage_name, t) ?? '—'}</strong>
                  <small>
                    {stageStatusLabel(event.stage_state, t)} ·{' '}
                    {formatRelative(event.recorded_at, locale, t('common.justNow'))}
                  </small>
                  {event.message ? <p>{event.message}</p> : null}
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {inspector.attemptQuery.data ? (
        <section>
          <h3>{t('admin.queueAttemptDetails')}</h3>
          <div className="node-metrics">
            <div>
              <span>{t('common.status')}</span>
              <strong>
                {queueStateLabel(inspector.attemptQuery.data.attempt.attempt_state, t)}
              </strong>
            </div>
            <div>
              <span>{t('admin.queueStage')}</span>
              <strong>
                {humanizeDocumentStage(inspector.attemptQuery.data.attempt.current_stage, t) ?? '—'}
              </strong>
            </div>
            <div>
              <span>{t('admin.queueInspectorProgress')}</span>
              <strong>{inspector.attemptQuery.data.attempt.progress_percent}%</strong>
            </div>
          </div>
        </section>
      ) : null}

      {item.failureMessage ? (
        <div className="failure-card">
          <strong>{item.failureCode ?? t('admin.queueFailure')}</strong>
          <p>{item.failureMessage}</p>
        </div>
      ) : null}
    </div>
  )
}

/**
 * Per-row queue controls. Which of the four buttons appear is a decision per
 * job, and holding it inside the screen put four conditional renders — and
 * every one of their handlers — inside the page component's own complexity.
 */
function QueueItemActions({
  item,
  mutations,
}: Readonly<{
  item: QueueItem
  mutations: ReturnType<typeof useQueueMutations>
}>) {
  const { t } = useTranslation()
  return (
    <div className="row-actions queue-item-actions">
      {item.canPause ? (
        <Button
          size="icon"
          variant="ghost"
          disabled={mutations.pauseState.isPending}
          onClick={(event) => {
            event.stopPropagation()
            mutations.pause(item.jobId)
          }}
          title={t('admin.queuePauseJob')}
        >
          <CirclePause size={15} />
        </Button>
      ) : null}
      {item.canResume ? (
        <Button
          size="icon"
          variant="ghost"
          disabled={mutations.resumeState.isPending}
          onClick={(event) => {
            event.stopPropagation()
            mutations.resume(item.jobId)
          }}
          title={t('admin.queueResumeJob')}
        >
          <CirclePlay size={15} />
        </Button>
      ) : null}
      {item.canCancel ? (
        <Button
          size="icon"
          variant="ghost"
          disabled={mutations.cancelState.isPending}
          onClick={(event) => {
            event.stopPropagation()
            mutations.cancel(item.jobId)
          }}
          title={t('admin.queueCancelJob')}
        >
          <Square size={14} />
        </Button>
      ) : null}
      {item.canRetryRequeue || item.hasStaleQueueLease ? (
        <Button
          size="icon"
          variant="ghost"
          disabled={mutations.retryState.isPending}
          onClick={(event) => {
            event.stopPropagation()
            mutations.retry(item.jobId)
          }}
          title={t('admin.queueRetryRequeueJob')}
        >
          <RotateCcw size={14} />
        </Button>
      ) : null}
    </div>
  )
}

/**
 * Bulk actions for the checked jobs. Only the actions that at least one of the
 * selection can accept are offered — an always-visible button that fails on
 * half a selection is worse than no button.
 */
function QueueBulkBar({
  cancelIds,
  mutations,
  onClearSelection,
  pauseIds,
  resumeIds,
  retryIds,
  selectedCount,
}: Readonly<{
  cancelIds: string[]
  mutations: ReturnType<typeof useQueueMutations>
  onClearSelection: () => void
  pauseIds: string[]
  resumeIds: string[]
  retryIds: string[]
  selectedCount: number
}>) {
  const { t } = useTranslation()
  if (selectedCount === 0) return null
  return (
    <div className="queue-bulk-bar">
      <strong>{t('admin.queueSelected', { count: selectedCount })}</strong>
      {retryIds.length > 0 ? (
        <Button
          size="sm"
          disabled={mutations.bulkState.isPending}
          onClick={() => mutations.bulk('retry_requeue', retryIds)}
        >
          <RotateCcw size={14} />
          {t('admin.queueRetryRequeueSelected', { count: retryIds.length })}
        </Button>
      ) : null}
      {pauseIds.length > 0 ? (
        <Button
          size="sm"
          disabled={mutations.bulkState.isPending}
          onClick={() => mutations.bulk('pause', pauseIds)}
        >
          <CirclePause size={14} />
          {t('admin.queuePauseSelected', { count: pauseIds.length })}
        </Button>
      ) : null}
      {resumeIds.length > 0 ? (
        <Button
          size="sm"
          disabled={mutations.bulkState.isPending}
          onClick={() => mutations.bulk('resume', resumeIds)}
        >
          <CirclePlay size={14} />
          {t('admin.queueResumeSelected', { count: resumeIds.length })}
        </Button>
      ) : null}
      {cancelIds.length > 0 ? (
        <Button
          size="sm"
          variant="danger"
          disabled={mutations.bulkState.isPending}
          onClick={() => mutations.bulk('cancel', cancelIds)}
        >
          <Square size={14} />
          {t('admin.queueCancelSelected', { count: cancelIds.length })}
        </Button>
      ) : null}
      <Button size="sm" variant="ghost" onClick={onClearSelection}>
        {t('admin.queueClearSelection')}
      </Button>
    </div>
  )
}

function QueuePageHeader({ libraryName }: Readonly<{ libraryName: string | undefined }>) {
  const { t } = useTranslation()
  return (
    <>
      <PageHeader
        eyebrow={t('admin.librariesTitle')}
        title={t('admin.queueTitle')}
        description={
          libraryName
            ? t('admin.queueLibrarySubtitle', { name: libraryName })
            : t('admin.queueSubtitle')
        }
      />
      <Link to="/admin/libraries" className="table-link">
        {t('admin.queueBackToLibraries')}
      </Link>
    </>
  )
}

/**
 * The queue row-actions cell. It lives at module scope so the service column
 * references a stable renderer instead of a component rebuilt inside
 * `QueueAdmin`; the mutations object travels as plain data.
 */
function renderQueueItemActions(item: QueueItem, mutations: ReturnType<typeof useQueueMutations>) {
  return <QueueItemActions item={item} mutations={mutations} />
}

export default function QueueAdmin() {
  const { t } = useTranslation()
  const { activeLibrary, activeWorkspace } = useApp()
  const [params, setParams] = useQueuedSearchParams()
  const { explicitLibraryId, scope, stateFilter } = resolveQueueRoute(
    params,
    activeLibrary?.id,
    activeWorkspace?.id,
  )
  const library = useQueueLibrary(scope.libraryId)
  const queue = useIngestQueue(scope)
  const queuePhase = queueScreenPhase(explicitLibraryId, queue, library)
  const mutations = useQueueMutations(scope)
  const [manualRefreshPending, setManualRefreshPending] = useState(false)
  const selectedJobId = params.get('jobId')
  const setSelectedJobId = useCallback(
    (jobId: string | null) => {
      setParams((next) => {
        if (jobId) next.set('jobId', jobId)
        else next.delete('jobId')
        return next
      })
    },
    [setParams],
  )
  const [checkedJobIds, setCheckedJobIds] = useState<Set<string>>(() => new Set())
  const items = useMemo(() => queue.data?.items ?? [], [queue.data?.items])
  const selectedItem = items.find((item) => item.jobId === selectedJobId) ?? null
  // The inspector renders only from a validated queue response. TanStack keeps
  // the last successful `data` while a poll's `select` throws, so a stale row's
  // movement controls would otherwise stay reachable over the error grid.
  const inspectorItem = queuePhase === 'ready' ? selectedItem : null

  // The grid filter selection is the online source of truth for the queue
  // state column: a named bucket is stored under the `status` alias
  // (`?status=running`), a custom multi-value selection under `filters`.
  const gridState = useMemo(() => queueGridState(params), [params])

  const applyGridState = useCallback(
    (update: SetStateAction<GridState>) => {
      setParams((nextParams) => {
        const next = typeof update === 'function' ? update(queueGridState(nextParams)) : update
        const status = queueBucketFor(next.filters.queue_state)
        const nextFilters = { ...next.filters }
        // A named bucket lives under `status`; only a custom selection stays in
        // `filters.queue_state`, so the two never both carry the same value.
        if (status !== 'all') delete nextFilters.queue_state
        const put = (key: string, value: string | null) => {
          if (value == null || value === '') nextParams.delete(key)
          else nextParams.set(key, value)
        }
        put('q', next.search)
        put('sort', next.sort ? `${next.sort.columnId}:${next.sort.direction}` : null)
        put('filters', serializeQueueFilters(nextFilters))
        put('status', status === 'all' ? null : status)
        put('pageSize', next.pageSize === DEFAULT_QUEUE_PAGE_SIZE ? null : String(next.pageSize))
        put('page', next.page > 1 ? String(next.page) : null)
        return nextParams
      })
    },
    [setParams],
  )

  const setStateFilter = (bucket: QueueStateFilter) => {
    applyGridState((current) => {
      const nextFilters = { ...current.filters }
      if (bucket === 'all') delete nextFilters.queue_state
      else nextFilters.queue_state = queueBucketKeys(bucket)
      return { ...current, filters: nextFilters, page: 1 }
    })
  }

  const toggleChecked = (jobId: string) => {
    setCheckedJobIds((current) => {
      const next = new Set(current)
      if (next.has(jobId)) next.delete(jobId)
      else next.add(jobId)
      return next
    })
  }
  const toggleAllVisible = (checked: boolean, rows: QueueItem[]) => {
    setCheckedJobIds((current) => {
      const next = new Set(current)
      for (const item of rows) {
        if (checked) next.add(item.jobId)
        else next.delete(item.jobId)
      }
      return next
    })
  }

  const selectedItems = items.filter((item) => checkedJobIds.has(item.jobId))
  const retryIds = selectedItems
    .filter((item) => item.canRetryRequeue || item.hasStaleQueueLease)
    .map((item) => item.jobId)
  const pauseIds = selectedItems.filter((item) => item.canPause).map((item) => item.jobId)
  const resumeIds = selectedItems.filter((item) => item.canResume).map((item) => item.jobId)
  const cancelIds = selectedItems.filter((item) => item.canCancel).map((item) => item.jobId)
  const mutationError = [
    mutations.bulkState.error,
    mutations.cancelState.error,
    mutations.moveState.error,
    mutations.pauseState.error,
    mutations.resumeState.error,
    mutations.retryState.error,
  ].find(Boolean)

  const refreshManually = async () => {
    setManualRefreshPending(true)
    try {
      await queue.refetch()
    } finally {
      setManualRefreshPending(false)
    }
  }

  const columns = useMemo(
    () => queueGridColumns({ onOpen: (item) => setSelectedJobId(item.jobId), t }),
    [setSelectedJobId, t],
  )
  const serviceColumns = useMemo<GridServiceColumn<QueueItem>[]>(
    () => [
      {
        id: 'actions',
        label: t('admin.queueActions'),
        align: 'end',
        cell: (item) => renderQueueItemActions(item, mutations),
      },
    ],
    [mutations, t],
  )

  const failedCount = items.filter((item) => item.queueState === 'failed').length
  const summary = queue.data?.summary

  return (
    <div className="admin-ops-page">
      <QueuePageHeader libraryName={library.data?.displayName} />
      {queuePhase === 'ready' && (
        <div className="queue-summary">
          {(
            [
              {
                count: summary?.running ?? 0,
                filter: 'running',
                label: t('admin.queueStateRunning'),
              },
              { count: summary?.queued ?? 0, filter: 'queued', label: t('admin.queueStateQueued') },
              { count: summary?.paused ?? 0, filter: 'paused', label: t('admin.queueStatePaused') },
              { count: failedCount, filter: 'failed', label: t('admin.queueFailure') },
              { count: summary?.total ?? 0, filter: 'all', label: t('common.all') },
            ] satisfies Array<{ count: number; filter: QueueStateFilter; label: string }>
          ).map((bucket) => (
            <button
              className={stateFilter === bucket.filter ? 'is-active' : ''}
              key={bucket.filter}
              onClick={() => setStateFilter(bucket.filter)}
              type="button"
            >
              <span>{bucket.label}</span>
              <strong>{bucket.count}</strong>
            </button>
          ))}
        </div>
      )}
      {queuePhase === 'ready' && mutationError ? (
        <div className="queue-inline-error" role="alert">
          {errorMessage(mutationError, t('common.error'))}
        </div>
      ) : null}
      {queuePhase === 'ready' ? (
        <QueueBulkBar
          cancelIds={cancelIds}
          mutations={mutations}
          onClearSelection={() => setCheckedJobIds(new Set())}
          pauseIds={pauseIds}
          resumeIds={resumeIds}
          retryIds={retryIds}
          selectedCount={checkedJobIds.size}
        />
      ) : null}
      <Panel>
        <DataGrid<QueueItem>
          rows={items}
          rowId={(item) => item.jobId}
          columns={columns}
          serviceColumns={serviceColumns}
          // Selection controls render only from a validated response, like the
          // inspector: a stale payload must not let an operator touch rows the
          // view no longer trusts. The selection itself survives a bad poll.
          {...(queuePhase === 'ready'
            ? {
                selection: {
                  mode: 'multiple' as const,
                  selectedIds: checkedJobIds,
                  onToggle: (item: QueueItem) => toggleChecked(item.jobId),
                  onToggleAll: (checked: boolean, rows: QueueItem[]) =>
                    toggleAllVisible(checked, rows),
                  headerLabel: t('admin.queueSelectAll'),
                  rowLabel: (item: QueueItem) =>
                    t('admin.queueSelectJob', { name: item.documentName }),
                },
              }
            : {})}
          state={gridState}
          onStateChange={applyGridState}
          ariaLabel={t('admin.queueTitle')}
          searchPlaceholder={t('admin.queueSearchPlaceholder')}
          emptyText={t('admin.queueEmpty')}
          noMatchText={t('admin.queueNoMatches')}
          {...(queuePhase === 'error'
            ? {
                errorText: queueErrorText(queue.error ?? library.error, t('admin.loadQueueFailed')),
                onRetry: () => {
                  void queue.refetch()
                  void library.refetch()
                },
              }
            : {})}
          pending={queuePhase === 'loading'}
          pageSizeOptions={QUEUE_PAGE_SIZE_OPTIONS}
          toolbarExtras={
            <>
              {queue.isFetching && !manualRefreshPending ? (
                <LoaderCircle
                  aria-label={t('common.loading')}
                  className="queue-polling-indicator spin"
                  size={15}
                />
              ) : null}
              <Button disabled={manualRefreshPending} onClick={() => void refreshManually()}>
                {t('common.refresh')}
              </Button>
            </>
          }
        />
      </Panel>
      <Drawer
        open={Boolean(inspectorItem)}
        onOpenChange={(open) => {
          if (!open) setSelectedJobId(null)
        }}
        title={inspectorItem?.documentName ?? t('admin.queueInspectorTitle')}
      >
        {inspectorItem ? (
          <QueueInspector
            item={inspectorItem}
            movePending={mutations.moveState.isPending}
            onMove={(direction) => mutations.move(inspectorItem.jobId, direction)}
          />
        ) : null}
      </Drawer>
    </div>
  )
}
