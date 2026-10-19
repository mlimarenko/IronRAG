import { ChevronRight, Columns3, RefreshCw, X } from 'lucide-react'
import { useMemo } from 'react'
import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'

import { DataGrid, type GridServiceColumn } from '@/shared/data-grid'
import { formatCost, type CostSummary } from '@/shared/format'
import { uploadStateLabel } from '@/shared/lib/enumLabels'
import { screenPhase } from '@/shared/lib/screenPhase'
import type { DocumentItem, Locale } from '@/shared/types'
import { Badge, Button, IconButton, Panel, Progress, Segmented } from '@/shared/ui'

import type { UploadQueueController } from './components/documents-page/useUploadQueueController'
import type { useDocumentsListController } from './components/documents-page/useDocumentsListController'
import type { useDocumentsQueries } from './components/documents-page/useDocumentsQueries'
import { DocumentsFilterNotice } from './components/documents-page/DocumentsFilterNotice'
import {
  documentsEmptyMessageKey,
  isIndexingFilterBucket,
  PAGE_SIZE_OPTIONS,
  UNION_STATUS_BUCKETS,
  type useDocumentsPageUrlState,
} from './components/documents-page/documentsPageState'
import { canReprocessDocument } from './model/documentAdapter'
import { documentGridColumns, documentSelectedFacets } from './model/documentsGrid'

type Props = {
  canWrite: boolean
  documents: ReturnType<typeof useDocumentsQueries>
  list: ReturnType<typeof useDocumentsListController>
  locale: Locale
  showDetailColumns: boolean
  uploads: UploadQueueController
  url: ReturnType<typeof useDocumentsPageUrlState>
}

const STATUS_BUCKETS = [
  'all',
  'ready',
  'notready',
  'inflight',
  'vectorgap',
  'graphgap',
  'graphready',
  'processing',
  'queued',
  'failed',
  'canceled',
  'empty',
] as const

function selectAllMatchingLabel(t: TFunction, expanding: boolean, total: number | null): string {
  if (expanding) return t('documents.selectAllBannerExpanding')
  if (total == null) return t('documents.selectAllBannerActionUnknown')
  return t('documents.selectAllBannerAction', { total })
}

/**
 * The per-row action buttons. Module level so the grid's `cell` render is not a
 * component defined inside `DocumentList`, and so the permission and event
 * wiring reads once.
 */
function DocumentRowActions({
  document,
  canWrite,
  canReprocess,
  onReprocess,
  onOpen,
  t,
}: Readonly<{
  document: DocumentItem
  canWrite: boolean
  canReprocess: boolean
  onReprocess: (documentId: string) => void
  onOpen: (document: DocumentItem) => void
  t: TFunction
}>) {
  return (
    <span className="document-row-actions">
      {canWrite && canReprocess && (
        <IconButton
          label={t('documents.reprocess')}
          onClick={(event) => {
            event.stopPropagation()
            onReprocess(document.id)
          }}
        >
          <RefreshCw size={16} />
        </IconButton>
      )}
      <IconButton
        label={t('documents.openDocument')}
        onClick={(event) => {
          event.stopPropagation()
          onOpen(document)
        }}
      >
        <ChevronRight size={16} />
      </IconButton>
    </span>
  )
}

/** The service column of row actions, built outside the component body. */
function documentActionsColumn({
  canWrite,
  t,
  onReprocess,
  onOpen,
}: Readonly<{
  canWrite: boolean
  t: TFunction
  onReprocess: (documentId: string) => void
  onOpen: (document: DocumentItem) => void
}>): GridServiceColumn<DocumentItem> {
  return {
    id: 'actions',
    label: t('documents.openDocument'),
    align: 'end',
    cell: (document) => (
      <DocumentRowActions
        document={document}
        canWrite={canWrite}
        canReprocess={canReprocessDocument(document)}
        onReprocess={onReprocess}
        onOpen={onOpen}
        t={t}
      />
    ),
  }
}

export function DocumentList({
  canWrite,
  documents,
  list,
  locale,
  showDetailColumns,
  uploads,
  url,
}: Readonly<Props>) {
  const { t } = useTranslation()
  const counts = documents.statusCounts
  const selectDoc = documents.selectDoc
  // `null` where the status aggregate holds no answer for a bucket. The
  // indexing sets are resolved by the server against an indexing invariant, not
  // counted per status, and a chip reading `· 0` on the set the operator was
  // just sent to would be a plain lie.
  const countFor = (status: (typeof STATUS_BUCKETS)[number]): number | null => {
    if (status === 'all') return counts?.total ?? documents.totalCount ?? documents.items.length
    if (status === 'notready')
      return (
        (counts?.processing ?? 0) +
        (counts?.queued ?? 0) +
        (counts?.failed ?? 0) +
        (counts?.canceled ?? 0) +
        (counts?.empty ?? 0)
      )
    if (status === 'inflight') return (counts?.processing ?? 0) + (counts?.queued ?? 0)
    if (status === 'vectorgap' || status === 'graphgap' || status === 'graphready') return null
    return counts?.[status] ?? 0
  }
  const statuses = STATUS_BUCKETS.filter(
    (status) =>
      status === 'all' ||
      status === url.statusBucket ||
      // A union bucket overlaps the primitive chips beside it, and would show
      // a count identical to one of them whenever only one member has
      // documents. It is where a counted figure elsewhere in the product
      // links to, so it stays visible while the reader is in it — and the
      // primitive buckets narrow from there — without crowding the row the
      // rest of the time.
      (!UNION_STATUS_BUCKETS.includes(status) && (countFor(status) ?? 0) > 0),
  ).map((status) => {
    const label =
      status === 'all'
        ? t('documents.all')
        : t(`documents.status${status[0]?.toUpperCase()}${status.slice(1)}`)
    const count = documents.isScopePending ? null : countFor(status)
    return { value: status, label: count == null ? label : `${label} · ${count}` }
  })
  const reprocessableItems = documents.items.filter((document) => canReprocessDocument(document))

  const processingClockMs = documents.snapshotAt ? Date.parse(documents.snapshotAt) : Number.NaN

  const columns = useMemo(
    () =>
      documentGridColumns({
        locale,
        t,
        showDetailColumns,
        onOpen: (document) => selectDoc(document),
        processingClockMs,
      }),
    [processingClockMs, locale, selectDoc, showDetailColumns, t],
  )
  const selectedFacets = useMemo(
    () => documentSelectedFacets(url.gridState.filters, t, locale),
    [locale, t, url.gridState.filters],
  )
  const serviceColumns = useMemo<GridServiceColumn<DocumentItem>[]>(
    () => [
      documentActionsColumn({
        canWrite,
        t,
        onReprocess: (documentId) => void list.reprocessDocument(documentId),
        onOpen: selectDoc,
      }),
    ],
    [canWrite, list, selectDoc, t],
  )

  // The unsorted list counts are the only signal an empty filtered set is a
  // filter result and not an empty library; the grid below states it too, and
  // both read the same sentence.
  const emptyMessage = t(
    documentsEmptyMessageKey({ searchQuery: url.searchQuery, statusBucket: url.statusBucket }),
  )
  const hasRows = documents.items.length > 0
  const phase = screenPhase({
    loading: documents.isLoading && !hasRows,
    failed: Boolean(documents.loadError) && !hasRows,
    empty: !hasRows,
  })

  return (
    <>
      <div className="cost-strip" aria-label={t('documents.expenses')}>
        <span>{t('documents.expenses')}:</span>
        <CostCell label={t('documents.costLibrary')} summary={documents.libraryCostSummary} />
        <span aria-hidden="true">·</span>
        <CostCell label={t('documents.costWorkspace')} summary={documents.workspaceCostSummary} />
      </div>
      {isIndexingFilterBucket(url.statusBucket) && (
        <DocumentsFilterNotice
          bucket={url.statusBucket}
          canWrite={canWrite}
          onSelectReprocessable={() =>
            list.setSelectedIds(new Set(reprocessableItems.map((document) => document.id)))
          }
          reprocessableCount={reprocessableItems.length}
          visibleCount={documents.items.length}
        />
      )}
      {list.bulkProgress && (
        <BulkProgress progress={list.bulkProgress} onDismiss={() => list.setBulkRerun(null)} />
      )}
      {list.showSelectAllMatching && (
        <div className="select-all-banner">
          <span>{t('documents.selectAllBannerSelected', { count: list.selectedIds.size })}</span>
          <Button
            size="sm"
            disabled={list.expandingSelection}
            onClick={() => void list.selectAllMatching()}
          >
            {selectAllMatchingLabel(t, list.expandingSelection, documents.filteredTotal)}
          </Button>
        </div>
      )}
      {list.selectedIds.size > 0 && (
        <div className="bulk-bar">
          <strong>{t('documents.nSelected', { count: list.selectedIds.size })}</strong>
          {canWrite && (
            <>
              <Button size="sm" onClick={() => void list.handleBulkCancel()}>
                {t('documents.bulkCancel')}
              </Button>
              <Button size="sm" onClick={() => void list.handleBulkReprocess()}>
                {t('documents.bulkRetry')}
              </Button>
              <Button size="sm" variant="danger" onClick={() => void list.handleBulkDelete()}>
                {t('documents.bulkDelete')}
              </Button>
            </>
          )}
          <IconButton label={t('documents.clearSelection')} onClick={list.clearSelection}>
            <X size={15} />
          </IconButton>
        </div>
      )}
      <Panel>
        {uploads.pendingUploads.length > 0 && (
          <div className="upload-file-list">
            {uploads.pendingUploads.map((upload) => (
              <div key={upload.id}>
                <span>
                  <strong>{upload.name}</strong>
                  {upload.errorAction && <small>{upload.errorAction}</small>}
                </span>
                <Badge tone={upload.state === 'error' ? 'failed' : 'processing'}>
                  {uploadStateLabel(upload.state, t)}
                </Badge>
              </div>
            ))}
          </div>
        )}
        <DataGrid<DocumentItem>
          mode="remote"
          rows={documents.items}
          rowId={(document) => document.id}
          columns={columns}
          serviceColumns={serviceColumns}
          selection={{
            mode: 'multiple',
            selectedIds: list.selectedIds,
            onToggle: (document) => list.toggleSelection(document.id),
            onToggleAll: (checked, rows) => list.toggleAllVisible(checked, rows),
            headerLabel: t('documents.selectAllVisible'),
            rowLabel: (document) =>
              t('documents.selectRow', { name: document.fileName ?? t('common.notAvailable') }),
          }}
          state={url.gridState}
          onStateChange={url.applyGridState}
          ariaLabel={t('documents.title')}
          searchPlaceholder={t('documents.searchPlaceholder')}
          emptyText={emptyMessage}
          noMatchText={emptyMessage}
          {...(phase === 'error' && documents.loadError
            ? { errorText: documents.loadError, onRetry: () => void documents.loadFirstPage() }
            : {})}
          pending={phase === 'loading' || documents.isScopePending}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          total={documents.isScopePending ? null : documents.filteredTotal}
          facets={selectedFacets}
          serverFacets={documents.serverFacets}
          pagination={{
            canPrevious: documents.pagination.canGoPrevious,
            canNext: documents.pagination.canGoNext,
            onPrevious: documents.pagination.goToPreviousPage,
            onNext: documents.pagination.goToNextPage,
            shown: hasRows
              ? {
                  from: documents.pagination.visibleRangeStart,
                  to: documents.pagination.visibleRangeEnd,
                }
              : null,
          }}
          toolbarExtras={
            <>
              <Button aria-pressed={showDetailColumns} onClick={list.toggleDetailColumns}>
                <Columns3 size={15} />
                {t('documents.additionalColumns')}
              </Button>
              <Segmented
                value={url.statusBucket}
                onChange={(status) =>
                  url.updateSearchParamState({
                    status: status === 'all' ? null : status,
                    documentId: null,
                  })
                }
                label={t('documents.statusFilter')}
                items={statuses}
              />
            </>
          }
        />
      </Panel>
    </>
  )
}

function CostCell({ label, summary }: Readonly<{ label: string; summary: CostSummary | null }>) {
  const { t, i18n } = useTranslation()
  const unknownLabel = t('common.costUnknown')
  const updatingLabel = t('documents.costUpdating')
  // While the owning rollup reconciles the backend withholds the amount; the
  // cell names the state instead of printing a stale or fake value and makes no
  // promise about when it will settle.
  const reconciling = summary?.costRollupFreshness === 'reconciling'
  const unpricedCount = summary?.unpricedProviderCallCount ?? 0
  const unpricedSuffix =
    !reconciling && summary && unpricedCount > 0
      ? ` · ${unpricedCount} ${t('documents.unpriced')}`
      : ''
  let formatted = unknownLabel
  let title = `${label}: ${unknownLabel}`
  if (reconciling) {
    formatted = updatingLabel
    title = `${label}: ${updatingLabel}`
  } else if (summary) {
    formatted = formatCost(summary, unknownLabel, t('documents.unpriced'), i18n.language).split(
      ' · ',
    )[0]!
    if (summary.totalCost && summary.currencyCode) {
      title = `${label}: ${summary.totalCost} ${summary.currencyCode}${unpricedSuffix}`
    }
  }
  return (
    <span className="cost-strip__item" title={title}>
      <span>{label}</span> <strong>{formatted}</strong>
    </span>
  )
}

function BulkProgress({
  progress,
  onDismiss,
}: Readonly<{
  progress: NonNullable<ReturnType<typeof useDocumentsListController>['bulkProgress']>
  onDismiss: () => void
}>) {
  const { t } = useTranslation()
  const settled = progress.completed + progress.failed
  const done = ['ready', 'failed', 'canceled'].includes(progress.status)
  const bulkLabel = done
    ? {
        delete: t('documents.bulkDeleteDone', { total: progress.total }),
        reprocess: t('documents.bulkRerunDone', { total: progress.total }),
      }
    : {
        delete: t('documents.bulkDeleteInFlight', { settled, total: progress.total }),
        reprocess: t('documents.bulkRerunInFlight', { settled, total: progress.total }),
      }
  return (
    <div className="bulk-progress">
      <div>
        <strong>{bulkLabel[progress.kind]}</strong>
        <small>
          {progress.failed > 0 ? `${progress.failed} ${t('common.failed')}` : progress.operationId}
        </small>
      </div>
      <Progress
        value={progress.total ? (settled / progress.total) * 100 : 0}
        label={t('documents.pipeline')}
      />
      {done && (
        <Button size="sm" variant="ghost" onClick={onDismiss}>
          {t('documents.bulkRerunDismiss')}
        </Button>
      )}
    </div>
  )
}
