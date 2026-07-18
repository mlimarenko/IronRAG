import { memo, type Dispatch, type SetStateAction } from 'react'
import type { TFunction } from 'i18next'
import { ArrowDown, ArrowUp, ChevronsUpDown, File, Globe, Loader2, XCircle } from 'lucide-react'

import type { DocumentListSortKey, DocumentListSortOrder } from '@/shared/api'
import { Checkbox } from '@/shared/components/ui/checkbox'
import { StatusBadge } from '@/shared/components/StatusBadge'
import type { DocumentItem, Locale } from '@/shared/types'

import {
  buildDocumentStatusBadgeConfig,
  formatDate,
  formatDocumentTypeLabel,
  getDocumentProcessingDurationMs,
  isWebPageDocument,
} from '@/features/documents/model/documentAdapter'

import type { LocalSortKey, LocalSortState, UploadQueueItem } from './documentsPageState'

type DocumentsTableProps = Readonly<{
  documents: DocumentItem[]
  items: DocumentItem[]
  locale: Locale
  localSort: LocalSortState
  onSelectDoc: (doc: DocumentItem) => void
  onToggleLocalSort: (key: LocalSortKey) => void
  onToggleSelection: (id: string) => void
  onToggleSortDirection: (target: DocumentListSortKey) => void
  pendingUploads: UploadQueueItem[]
  processingClockMs: number
  selectedDocId: string | null
  selectedIds: Set<string>
  selectionMode: boolean
  setSelectedIds: Dispatch<SetStateAction<Set<string>>>
  showDetailColumns: boolean
  sortBy: DocumentListSortKey
  sortOrder: DocumentListSortOrder
  t: TFunction
}>

const EMPTY_VALUE = '—'

export function DocumentsTable({
  documents,
  items,
  locale,
  localSort,
  onSelectDoc,
  onToggleLocalSort,
  onToggleSelection,
  onToggleSortDirection,
  pendingUploads,
  processingClockMs,
  selectedDocId,
  selectedIds,
  selectionMode,
  setSelectedIds,
  showDetailColumns,
  sortBy,
  sortOrder,
  t,
}: DocumentsTableProps) {
  const allVisibleSelected = items.length > 0 && items.every((doc) => selectedIds.has(doc.id))
  // The base table fits comfortably in ~760px (Name/Type/Uploaded/Status). The
  // page-scoped detail columns (Cost/Pipeline/Finished) only widen the grid
  // when the operator opts in, so laptops stop horizontally scrolling by default.
  const minWidth = showDetailColumns ? 'min-w-[1040px]' : 'min-w-[760px]'
  const detailColCount = showDetailColumns ? 3 : 0
  const placeholderSpan = 2 + detailColCount // Type + Uploaded + opt-in detail columns

  return (
    <>
      <div className="space-y-3 p-3 xl:hidden">
        {pendingUploads.map((upload) => (
          <PendingUploadCard key={`upload-card-${upload.name}`} t={t} upload={upload} />
        ))}
        {documents.map((doc) => (
          <DocumentCard
            key={doc.id}
            doc={doc}
            isCursor={selectedDocId === doc.id}
            locale={locale}
            onSelectDoc={onSelectDoc}
            onToggleSelection={onToggleSelection}
            processingClockMs={processingClockMs}
            selected={selectedIds.has(doc.id)}
            selectionMode={selectionMode}
            showDetailColumns={showDetailColumns}
            t={t}
          />
        ))}
      </div>
      <table className={`hidden w-full ${minWidth} table-fixed text-sm xl:table`}>
        <colgroup>
          {selectionMode && <col className="w-12" />}
          <col />
          <col className="w-28" />
          <col className="w-36" />
          {showDetailColumns && <col className="w-24" />}
          {showDetailColumns && <col className="w-28" />}
          {showDetailColumns && <col className="w-36" />}
          <col className="w-52" />
        </colgroup>
        <thead className="sticky top-0 z-10 bg-card">
          <tr className="border-b text-left">
            {selectionMode && (
              <th className="px-4 py-3 w-10">
                <Checkbox
                  aria-label={t('documents.selectAllVisible')}
                  checked={allVisibleSelected}
                  onCheckedChange={() =>
                    setSelectedIds((prev) => {
                      const next = new Set(prev)
                      for (const doc of items) {
                        if (allVisibleSelected) next.delete(doc.id)
                        else next.add(doc.id)
                      }
                      return next
                    })
                  }
                />
              </th>
            )}
            <SortHeader
              active={sortBy === 'file_name'}
              order={sortOrder}
              label={t('documents.name')}
              onClick={() => onToggleSortDirection('file_name')}
            />
            <SortHeader
              active={sortBy === 'file_type'}
              order={sortOrder}
              label={t('documents.type')}
              onClick={() => onToggleSortDirection('file_type')}
            />
            <SortHeader
              active={sortBy === 'uploaded_at'}
              order={sortOrder}
              label={t('documents.uploaded')}
              onClick={() => onToggleSortDirection('uploaded_at')}
            />
            {showDetailColumns && (
              <>
                <LocalSortHeader
                  active={localSort?.key === 'cost'}
                  order={localSort?.direction ?? null}
                  label={t('documents.cost')}
                  hint={t('documents.pageLocalSortHint')}
                  onClick={() => onToggleLocalSort('cost')}
                />
                <LocalSortHeader
                  active={localSort?.key === 'time'}
                  order={localSort?.direction ?? null}
                  label={t('documents.pipelineTime')}
                  hint={t('documents.pageLocalSortHint')}
                  onClick={() => onToggleLocalSort('time')}
                />
                <LocalSortHeader
                  active={localSort?.key === 'finished'}
                  order={localSort?.direction ?? null}
                  label={t('documents.finished')}
                  hint={t('documents.pageLocalSortHint')}
                  onClick={() => onToggleLocalSort('finished')}
                />
              </>
            )}
            <SortHeader
              active={sortBy === 'status'}
              order={sortOrder}
              label={t('documents.status')}
              onClick={() => onToggleSortDirection('status')}
            />
          </tr>
        </thead>
        <tbody>
          {pendingUploads.map((upload) => {
            const uploadErrorTitle = [
              upload.error,
              upload.errorAction,
              upload.errorDiagnosticCode,
              upload.errorDiagnosticMessage,
            ]
              .filter(Boolean)
              .join('\n')

            return (
              <tr key={`upload-${upload.name}`} className="border-b opacity-80">
                {selectionMode && <td className="px-4 py-3 w-10" />}
                <td className="px-4 py-3">
                  <DocumentNameCell fileName={upload.name} />
                </td>
                <td
                  className="px-4 py-3 text-muted-foreground text-2xs"
                  colSpan={placeholderSpan}
                />
                <td className="px-4 py-3 max-w-[320px]">
                  {upload.state === 'error' ? (
                    <span
                      className="flex items-start gap-1.5 text-xs text-status-failed"
                      title={uploadErrorTitle || undefined}
                    >
                      <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                      <span className="min-w-0">
                        <span className="block truncate">
                          {upload.error ?? t('documents.uploadFailed')}
                        </span>
                        {upload.errorAction && (
                          <span className="mt-0.5 block truncate text-muted-foreground">
                            {upload.errorAction}
                          </span>
                        )}
                      </span>
                    </span>
                  ) : (
                    <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                      <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
                      {t('documents.uploading')}
                    </span>
                  )}
                </td>
              </tr>
            )
          })}
          {documents.map((doc) => (
            <DocumentRow
              key={doc.id}
              doc={doc}
              locale={locale}
              processingClockMs={processingClockMs}
              selected={selectedIds.has(doc.id)}
              isCursor={selectedDocId === doc.id}
              selectionMode={selectionMode}
              showDetailColumns={showDetailColumns}
              onSelectDoc={onSelectDoc}
              onToggleSelection={onToggleSelection}
              t={t}
            />
          ))}
        </tbody>
      </table>
    </>
  )
}

type DocumentRowProps = Readonly<{
  doc: DocumentItem
  locale: Locale
  processingClockMs: number
  selected: boolean
  isCursor: boolean
  selectionMode: boolean
  showDetailColumns: boolean
  onSelectDoc: (doc: DocumentItem) => void
  onToggleSelection: (id: string) => void
  t: TFunction
}>

function PendingUploadCard({ t, upload }: Readonly<{ t: TFunction; upload: UploadQueueItem }>) {
  const uploadErrorTitle = [
    upload.error,
    upload.errorAction,
    upload.errorDiagnosticCode,
    upload.errorDiagnosticMessage,
  ]
    .filter(Boolean)
    .join('\n')

  return (
    <article className="workbench-surface p-4">
      <DocumentNameCell fileName={upload.name} />
      <div className="mt-3">
        {upload.state === 'error' ? (
          <div
            className="rounded-lg bg-destructive/5 px-3 py-2 text-xs text-destructive"
            title={uploadErrorTitle || undefined}
          >
            <div className="font-bold">{upload.error ?? t('documents.uploadFailed')}</div>
            {upload.errorAction && (
              <div className="mt-1 text-muted-foreground">{upload.errorAction}</div>
            )}
          </div>
        ) : (
          <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
            <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
            {t('documents.uploading')}
          </span>
        )}
      </div>
    </article>
  )
}

function DocumentCard({
  doc,
  isCursor,
  locale,
  onSelectDoc,
  onToggleSelection,
  processingClockMs,
  selected,
  selectionMode,
  showDetailColumns,
  t,
}: DocumentRowProps) {
  const isWebPage = isWebPageDocument(doc.sourceKind, doc.sourceUri, doc.fileName)
  const typeLabel = formatDocumentTypeLabel(doc.fileType, doc.sourceKind, t, {
    sourceUri: doc.sourceUri,
    fileName: doc.fileName,
  })
  const processingDurationMs = getDocumentProcessingDurationMs(doc, processingClockMs)

  return (
    <article
      className={`workbench-surface p-4 transition-all ${documentRowSurfaceClass(selected, isCursor)}`}
    >
      <div className="flex items-start gap-3">
        {selectionMode && (
          <Checkbox
            aria-label={t('documents.selectRow', { name: doc.fileName })}
            checked={selected}
            className="mt-1"
            onCheckedChange={() => onToggleSelection(doc.id)}
          />
        )}
        <button
          aria-current={isCursor ? 'true' : undefined}
          type="button"
          className="min-w-0 flex-1 text-left"
          onClick={() => (selectionMode ? onToggleSelection(doc.id) : onSelectDoc(doc))}
        >
          <div className="flex min-w-0 items-start justify-between gap-3">
            <DocumentNameCell
              fileName={doc.fileName}
              isWebPage={isWebPage}
              sourceUri={doc.sourceUri}
            />
            <DocumentStatusBadge doc={doc} t={t} />
          </div>
        </button>
      </div>
      <div className="mt-4 grid grid-cols-2 gap-2 rounded-lg bg-surface-sunken/60 p-2 text-xs">
        <MobileDocumentMetric label={t('documents.type')} value={typeLabel} />
        <MobileDocumentMetric
          label={t('documents.uploaded')}
          value={formatDate(doc.uploadedAt, locale)}
        />
        {showDetailColumns && (
          <>
            <MobileDocumentMetric
              label={t('documents.cost')}
              value={doc.cost != null ? `$${doc.cost.toFixed(3)}` : EMPTY_VALUE}
            />
            <MobileDocumentMetric
              label={t('documents.pipelineTime')}
              value={
                processingDurationMs != null
                  ? `${Math.floor(processingDurationMs / 1000)}s`
                  : EMPTY_VALUE
              }
            />
          </>
        )}
      </div>
    </article>
  )
}

function MobileDocumentMetric({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <div className="min-w-0">
      <div className="truncate section-label">{label}</div>
      <div className="mt-0.5 truncate font-semibold" title={value}>
        {value}
      </div>
    </div>
  )
}

function documentRowSurfaceClass(selected: boolean, isCursor: boolean): string {
  if (selected) return 'border-primary/30 bg-primary/10'
  if (isCursor) return 'border-primary/40 bg-primary/5'
  return 'border-border/70'
}

function desktopDocumentRowClass(selected: boolean, isCursor: boolean): string {
  if (selected) return 'bg-primary/10'
  if (isCursor) return 'bg-primary/5 border-l-2 border-l-primary'
  return 'hover:bg-accent/30'
}

function isActiveProcessingDoc(doc: DocumentItem): boolean {
  return doc.status === 'processing' || doc.status === 'queued'
}

function DocumentRowImpl({
  doc,
  locale,
  processingClockMs,
  selected,
  isCursor,
  selectionMode,
  showDetailColumns,
  onSelectDoc,
  onToggleSelection,
  t,
}: DocumentRowProps) {
  const isWebPage = isWebPageDocument(doc.sourceKind, doc.sourceUri, doc.fileName)
  const typeLabel = formatDocumentTypeLabel(doc.fileType, doc.sourceKind, t, {
    sourceUri: doc.sourceUri,
    fileName: doc.fileName,
  })
  const processingDurationMs = getDocumentProcessingDurationMs(doc, processingClockMs)
  return (
    <tr
      className={`border-b transition-all duration-150 ${desktopDocumentRowClass(selected, isCursor)}`}
    >
      {selectionMode && (
        <td className="px-4 py-3 w-10">
          <Checkbox
            aria-label={t('documents.selectRow', { name: doc.fileName })}
            checked={selected}
            onClick={(event) => event.stopPropagation()}
            onCheckedChange={() => onToggleSelection(doc.id)}
          />
        </td>
      )}
      <td className="px-4 py-3">
        {selectionMode ? (
          <DocumentNameCell
            fileName={doc.fileName}
            isWebPage={isWebPage}
            sourceUri={doc.sourceUri}
          />
        ) : (
          <button
            aria-current={isCursor ? 'true' : undefined}
            className="w-full text-left"
            type="button"
            onClick={() => onSelectDoc(doc)}
          >
            <DocumentNameCell
              fileName={doc.fileName}
              isWebPage={isWebPage}
              sourceUri={doc.sourceUri}
            />
          </button>
        )}
      </td>
      <td
        className={`px-4 py-3 text-muted-foreground text-2xs font-bold tracking-widest ${isWebPage ? '' : 'uppercase'}`}
        title={typeLabel}
      >
        {typeLabel}
      </td>
      <td className="px-4 py-3 text-muted-foreground text-xs">
        {formatDate(doc.uploadedAt, locale)}
      </td>
      {showDetailColumns && (
        <>
          <td className="px-4 py-3 text-muted-foreground tabular-nums text-xs">
            {doc.cost != null ? `$${doc.cost.toFixed(3)}` : EMPTY_VALUE}
          </td>
          <td className="px-4 py-3 text-muted-foreground tabular-nums text-xs">
            {processingDurationMs != null
              ? `${Math.floor(processingDurationMs / 1000)}s`
              : EMPTY_VALUE}
          </td>
          <td className="px-4 py-3 text-muted-foreground text-xs">
            {doc.processingFinishedAt ? formatDate(doc.processingFinishedAt, locale) : EMPTY_VALUE}
          </td>
        </>
      )}
      <td className="px-4 py-3">
        <DocumentStatusBadge doc={doc} t={t} />
      </td>
    </tr>
  )
}

// The processing clock ticks every second while any document is ingesting,
// which previously re-rendered every row in the (up to 1000-row) page each
// second. Rows that are not actively processing render nothing clock-derived,
// so we skip their re-render on a pure clock advance. Active rows still update
// their live duration. All other props are compared by identity.
const DocumentRow = memo(DocumentRowImpl, (prev, next) => {
  if (
    prev.doc !== next.doc ||
    prev.locale !== next.locale ||
    prev.selected !== next.selected ||
    prev.isCursor !== next.isCursor ||
    prev.selectionMode !== next.selectionMode ||
    prev.showDetailColumns !== next.showDetailColumns ||
    prev.onSelectDoc !== next.onSelectDoc ||
    prev.onToggleSelection !== next.onToggleSelection ||
    prev.t !== next.t
  ) {
    return false
  }
  // Only the live duration column reads the clock, and only for active docs.
  if (next.showDetailColumns && isActiveProcessingDoc(next.doc)) {
    return prev.processingClockMs === next.processingClockMs
  }
  return true
})

function DocumentStatusBadge({ doc, t }: Readonly<{ doc: DocumentItem; t: TFunction }>) {
  const statusBadgeConfig = buildDocumentStatusBadgeConfig(t)
  const badge = statusBadgeConfig[doc.status]
  const progress =
    doc.status === 'processing'
      ? Math.max(0, Math.min(99, Math.round(doc.progressPercent ?? 0)))
      : null
  const title =
    progress != null
      ? [badge.label, `${progress}%`, doc.statusReason].filter(Boolean).join(' · ')
      : doc.statusReason

  if (progress == null) {
    return (
      <StatusBadge tone={badge.tone} className="whitespace-nowrap" title={title}>
        {badge.label}
      </StatusBadge>
    )
  }

  return (
    <StatusBadge
      tone={badge.tone}
      className="relative isolate min-w-[9.25rem] justify-center overflow-hidden whitespace-nowrap"
      title={title}
      aria-label={`${badge.label} ${progress}%`}
    >
      <span
        aria-hidden="true"
        className="absolute inset-y-0 left-0 rounded-full transition-all duration-500"
        style={{
          width: `${progress}%`,
          background: 'hsl(var(--status-processing-ring) / 0.95)',
        }}
      />
      <span className="relative z-10 flex items-center justify-center gap-1.5 whitespace-nowrap">
        <span>{badge.label}</span>
        <span className="tabular-nums">{progress}%</span>
      </span>
    </StatusBadge>
  )
}

/**
 * Server-sort header. The whole list re-queries and re-paginates, so the active
 * column shows a solid up/down arrow; inactive columns show a faint
 * "sortable" affordance so the operator knows a click does something.
 */
function serverSortAriaValue(
  active: boolean,
  order: DocumentListSortOrder,
): 'ascending' | 'descending' | undefined {
  if (!active) return undefined
  return order === 'asc' ? 'ascending' : 'descending'
}

function SortHeader({
  active,
  order,
  label,
  onClick,
}: Readonly<{
  active: boolean
  order: DocumentListSortOrder
  label: string
  onClick: () => void
}>) {
  return (
    <th aria-sort={serverSortAriaValue(active, order)} className="px-4 py-3 section-label">
      <button
        type="button"
        className={`group flex items-center gap-1 transition-colors ${
          active ? 'text-foreground' : 'hover:text-foreground'
        }`}
        onClick={onClick}
      >
        {label}
        {sortIcon(active, order, 'text-primary')}
      </button>
    </th>
  )
}

/**
 * Page-scoped (in-memory) sort header. Visually distinct from server-sort
 * columns: a dashed underline + a "page" tag mark these as sorting only the
 * rows currently loaded, so the silent reset on pagination is no longer a
 * surprise. Pairs with the DocumentsFiltersBar "detail columns" toggle.
 */
function sortIcon(active: boolean, order: DocumentListSortOrder | null, activeClassName: string) {
  if (active && order === 'asc') return <ArrowUp className={`h-3.5 w-3.5 ${activeClassName}`} />
  if (active && order === 'desc') return <ArrowDown className={`h-3.5 w-3.5 ${activeClassName}`} />
  return (
    <ChevronsUpDown className="h-3.5 w-3.5 opacity-0 transition-opacity group-hover:opacity-40" />
  )
}

function LocalSortHeader({
  active,
  order,
  label,
  hint,
  onClick,
}: Readonly<{
  active: boolean
  order: 'asc' | 'desc' | null
  label: string
  hint: string
  onClick: () => void
}>) {
  return (
    <th className="px-4 py-3 section-label">
      <button
        type="button"
        className={`group flex items-center gap-1.5 transition-colors ${
          active ? 'text-foreground' : 'hover:text-foreground'
        }`}
        title={hint}
        onClick={onClick}
      >
        <span className="border-b border-dashed border-muted-foreground/40 leading-4">{label}</span>
        {sortIcon(active, order, 'text-accent-strong')}
      </button>
    </th>
  )
}

function DocumentNameCell({
  fileName,
  isWebPage = false,
  sourceUri,
}: Readonly<{
  fileName: string
  isWebPage?: boolean | undefined
  sourceUri?: string | undefined
}>) {
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div
        className={`w-8 h-8 rounded-xl flex items-center justify-center shrink-0 ${
          isWebPage ? 'bg-primary/10' : 'bg-surface-sunken'
        }`}
      >
        {isWebPage ? (
          <Globe className="h-3.5 w-3.5 text-primary" />
        ) : (
          <File className="h-3.5 w-3.5 text-muted-foreground" />
        )}
      </div>
      <div className="min-w-0 flex-1">
        <span className="block truncate font-semibold" title={fileName}>
          {fileName}
        </span>
        {isWebPage && sourceUri && sourceUri !== fileName && (
          <span className="block truncate text-2xs text-muted-foreground" title={sourceUri}>
            {sourceUri}
          </span>
        )}
      </div>
    </div>
  )
}
