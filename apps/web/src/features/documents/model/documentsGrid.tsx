import type { TFunction } from 'i18next'
import { FileCode2, FileSpreadsheet, FileText, Globe2 } from 'lucide-react'

import type { DocumentColumnFilters, DocumentFacetColumn } from '@/shared/api'
import {
  gridFacetKey,
  gridFacetValue,
  type GridColumn,
  type GridFacetOption,
  type GridState,
} from '@/shared/data-grid'
import { formatDate, formatDocumentSize } from '@/shared/format'
import { documentStatusLabel } from '@/shared/lib/enumLabels'
import type { DocumentItem, Locale } from '@/shared/types'
import { Badge } from '@/shared/ui'

import { getDocumentSourceFormat, getDocumentProcessingDurationMs } from './documentAdapter'

/**
 * The documents grid columns. Ids are the server's own vocabulary — the
 * `DocumentListSortKey`, the `DocumentFacetColumn` and the `status`/`filters`
 * parameter names all agree on `uploaded_at`, `file_name`, `file_type`,
 * `file_size`, `status`, `cost`, `pipeline_time`, `finished_at` — so a column
 * id maps to a facet request and a server sort without a lookup table that
 * could drift.
 */
export type DocumentGridColumnId = DocumentFacetColumn

/** A scalar document cell or facet value: a string, a number, or the null bucket. */
type GridScalar = string | number | null

/**
 * Which `DocumentColumnFilters` key a grid column id writes to. Every value the
 * grid stores is decoded from its typed facet key, so a null bucket stays a
 * real null and a decimal cost string keeps every digit it arrived with.
 */
const COLUMN_FILTER_FIELD: Record<DocumentGridColumnId, keyof DocumentColumnFilters> = {
  uploaded_at: 'uploadedAt',
  file_name: 'fileName',
  file_type: 'fileType',
  file_size: 'fileSize',
  status: 'status',
  cost: 'cost',
  pipeline_time: 'pipelineTime',
  finished_at: 'finishedAt',
}

export const DOCUMENT_GRID_COLUMNS: readonly DocumentGridColumnId[] = [
  'file_name',
  'file_type',
  'file_size',
  'uploaded_at',
  'status',
  'cost',
  'pipeline_time',
  'finished_at',
]

function scalarValues(keys: readonly string[]): Array<GridScalar> {
  // Facet keys are typed; a boolean can never appear in a document column, so
  // dropping one here is a guard, not a silent coercion.
  return keys.map(gridFacetValue).filter((value): value is GridScalar => typeof value !== 'boolean')
}

/** Grid filter selection -> the typed per-column filters the list and facet endpoints take. */
export function documentsColumnFilters(
  filters: GridState['filters'],
): DocumentColumnFilters | undefined {
  const result: DocumentColumnFilters = {}
  let any = false
  for (const [columnId, keys] of Object.entries(filters)) {
    if (keys.length === 0) continue
    const field = COLUMN_FILTER_FIELD[columnId as DocumentGridColumnId]
    if (!field) continue
    const values = scalarValues(keys)
    switch (columnId) {
      case 'uploaded_at':
        result.uploadedAt = values as NonNullable<DocumentColumnFilters['uploadedAt']>
        break
      case 'finished_at':
        result.finishedAt = values as NonNullable<DocumentColumnFilters['finishedAt']>
        break
      case 'file_name':
        result.fileName = values as NonNullable<DocumentColumnFilters['fileName']>
        break
      case 'file_type':
        result.fileType = values as NonNullable<DocumentColumnFilters['fileType']>
        break
      case 'file_size':
        result.fileSize = values as NonNullable<DocumentColumnFilters['fileSize']>
        break
      case 'cost':
        result.cost = values as NonNullable<DocumentColumnFilters['cost']>
        break
      case 'pipeline_time':
        result.pipelineTime = values as NonNullable<DocumentColumnFilters['pipelineTime']>
        break
      case 'status':
        result.status = values as NonNullable<DocumentColumnFilters['status']>
        break
      default:
        continue
    }
    any = true
  }
  return any ? result : undefined
}

/** One duration label shared by row cells and facet options. */
function formatDurationMs(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  const totalSeconds = Math.floor(ms / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  const minutePart = minutes ? `${minutes}m ` : ''
  return `${minutePart}${seconds}s`
}

/** The human label of one facet value, reusing the page's own formatters. */
export function documentFacetLabel(
  column: DocumentGridColumnId,
  value: GridScalar,
  t: TFunction,
  locale: Locale,
): string {
  if (value === null) return ''
  switch (column) {
    case 'status':
      return documentStatusLabel(String(value), t)
    case 'file_size':
      return formatDocumentSize(Number(value))
    case 'cost':
      return `$${value}`
    case 'pipeline_time':
      return formatDurationMs(Number(value))
    case 'uploaded_at':
    case 'finished_at':
      return formatDate(String(value), locale, {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
      })
    default:
      return String(value)
  }
}

/** Server facet rows -> typed grid options. The key is derived here, never trusted from the wire. */
export function documentFacetOptions(
  column: DocumentGridColumnId,
  values: ReadonlyArray<{ value: GridScalar; count: number }>,
  t: TFunction,
  locale: Locale,
): GridFacetOption[] {
  return values.map(({ value, count }) => ({
    key: gridFacetKey(value),
    value,
    label: documentFacetLabel(column, value, t, locale),
    ...(value === null ? {} : { title: String(value) }),
    count,
  }))
}

/**
 * Options for the active filters of a column, so the toolbar chip reads the
 * same translated/formatted label the menu offered. Counts are unknown here —
 * a chip is not a menu, and no number is printed for it.
 */
export function documentSelectedFacets(
  filters: GridState['filters'],
  t: TFunction,
  locale: Locale,
): Record<string, GridFacetOption[]> {
  const result: Record<string, GridFacetOption[]> = {}
  for (const [columnId, keys] of Object.entries(filters)) {
    if (keys.length === 0) continue
    result[columnId] = keys.map((key) => {
      const decoded = gridFacetValue(key)
      const value = typeof decoded === 'boolean' ? null : decoded
      return {
        key,
        value,
        label: documentFacetLabel(columnId as DocumentGridColumnId, value, t, locale),
        count: -1,
      }
    })
  }
  return result
}

export interface DocumentGridColumnOptions {
  locale: Locale
  t: TFunction
  showDetailColumns: boolean
  /** Open the inspector for a row; wired to the name cell's button. */
  onOpen: (document: DocumentItem) => void
  /**
   * The server snapshot clock, shared by the displayed duration and the
   * full-dataset sort and facets. It advances on an explicit refresh.
   */
  processingClockMs: number
}

function documentFileIcon(document: DocumentItem) {
  if (document.sourceKind === 'web_page') return <Globe2 size={15} aria-hidden="true" />
  const sourceFormat = getDocumentSourceFormat(document)
  if (['csv', 'tsv', 'xls', 'xlsx'].includes(sourceFormat)) {
    return <FileSpreadsheet size={15} aria-hidden="true" />
  }
  if (['yaml', 'yml', 'json', 'js', 'ts', 'py', 'rs', 'code', 'plain'].includes(sourceFormat)) {
    return <FileCode2 size={15} aria-hidden="true" />
  }
  return <FileText size={15} aria-hidden="true" />
}

export function documentGridColumns({
  locale,
  t,
  showDetailColumns,
  onOpen,
  processingClockMs,
}: DocumentGridColumnOptions): GridColumn<DocumentItem>[] {
  const columns: GridColumn<DocumentItem>[] = [
    {
      id: 'file_name',
      label: t('common.name'),
      value: (document) => document.fileName,
      cell: (document) => (
        <button
          type="button"
          className="document-row-open"
          // The row itself toggles selection in multiple mode; opening the
          // inspector is a distinct action and must not also select.
          onClick={(event) => {
            event.stopPropagation()
            onOpen(document)
          }}
        >
          {documentFileIcon(document)}
          <span className="cell-title">
            <strong>{document.fileName ?? t('common.notAvailable')}</strong>
            {document.externalKey !== document.fileName && <small>{document.externalKey}</small>}
          </span>
        </button>
      ),
    },
    {
      id: 'file_type',
      label: t('documents.type'),
      value: (document) => document.fileType,
      displayText: (document) => document.fileType ?? '—',
    },
    {
      id: 'file_size',
      label: t('documents.size'),
      align: 'end',
      value: (document) => document.fileSize,
      displayText: (document) =>
        document.fileSize == null
          ? '—'
          : documentFacetLabel('file_size', document.fileSize, t, locale),
    },
    {
      id: 'uploaded_at',
      label: t('documents.uploaded'),
      value: (document) => document.uploadedAt,
      cell: (document) => (
        <span title={formatDate(document.uploadedAt, locale)}>
          {documentFacetLabel('uploaded_at', document.uploadedAt, t, locale)}
        </span>
      ),
      displayText: (document) => documentFacetLabel('uploaded_at', document.uploadedAt, t, locale),
    },
    {
      id: 'status',
      label: t('common.status'),
      value: (document) => document.status,
      cell: (document) => (
        <Badge tone={document.status}>{documentStatusLabel(document.status, t)}</Badge>
      ),
      displayText: (document) => documentStatusLabel(document.status, t),
    },
  ]
  if (showDetailColumns) {
    columns.push(
      {
        id: 'cost',
        label: t('documents.cost'),
        align: 'end',
        value: (document) => document.cost,
        displayText: (document) => {
          if (document.cost != null) return documentFacetLabel('cost', document.cost, t, locale)
          // While the owning rollup reconciles the backend withholds the cost;
          // name that state instead of a bare dash that reads as "no cost".
          return document.costRollupFreshness === 'reconciling' ? t('documents.costUpdating') : '—'
        },
      },
      {
        id: 'pipeline_time',
        label: t('documents.pipelineTime'),
        align: 'end',
        value: (document) => getDocumentProcessingDurationMs(document, processingClockMs),
        displayText: (document) => {
          const duration = getDocumentProcessingDurationMs(document, processingClockMs)
          if (duration == null) return '—'
          return formatDurationMs(duration)
        },
      },
      {
        id: 'finished_at',
        label: t('documents.finished'),
        value: (document) => document.processingFinishedAt ?? null,
        displayText: (document) =>
          document.processingFinishedAt
            ? documentFacetLabel('finished_at', document.processingFinishedAt, t, locale)
            : '—',
      },
    )
  }
  return columns
}
