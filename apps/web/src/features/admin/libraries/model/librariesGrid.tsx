import type { TFunction } from 'i18next'
import { Download, MoreHorizontal, Settings2, Trash2, Upload } from 'lucide-react'

import type { AdminLibraryColumnFilters, AdminLibraryFacetColumn } from '@/shared/api'
import type { AdminLibraryCostValue } from '@/shared/api/generated'
import {
  gridFacetKey,
  gridFacetValue,
  type GridColumn,
  type GridFacetOption,
  type GridServiceColumn,
  type GridState,
} from '@/shared/data-grid'
import { formatCost, formatNumber } from '@/shared/format'
import { libraryLifecycleStateLabel } from '@/shared/lib/enumLabels'
import type { Locale } from '@/shared/types'
import { Badge, Button } from '@/shared/ui'
import {
  LibraryQueueLink,
  MissingBindingPurposeLabels,
} from '@/features/admin/libraries/components/libraryCells'

import type { AdminLibraryRow } from './librariesAdapter'
import { costValueKey, costValueToSummary, decodeCostValueKey, rowCostValue } from './libraryCost'

/**
 * The administration library grid columns. Ids are the server's own vocabulary:
 * the `AdminLibrarySortKey`, the `AdminLibraryColumnFilters` key and the
 * `AdminLibraryFacetColumn` all agree on `library`, `workspace`, `documents`,
 * `queue`, `cost`, `calls`, `readiness`, `lifecycle`, so a column id maps to a
 * facet request and a server sort without a lookup table that could drift.
 */
export type LibraryGridColumnId = AdminLibraryFacetColumn

export function libraryActionsColumn({
  t,
  onOpen,
  onConfigureAi,
  onBackup,
  onRestore,
  onDelete,
  onDetails,
  deleting,
}: {
  t: TFunction
  onOpen: (row: AdminLibraryRow) => Promise<void>
  onConfigureAi: (row: AdminLibraryRow) => void
  onBackup: (row: AdminLibraryRow) => void
  onRestore: (row: AdminLibraryRow) => void
  onDelete: (row: AdminLibraryRow) => Promise<void>
  onDetails: (row: AdminLibraryRow) => void
  deleting: boolean
}): GridServiceColumn<AdminLibraryRow> {
  return {
    id: 'actions',
    label: t('admin.libraries.actions'),
    align: 'end',
    cell: (row) => (
      <div className="row-actions">
        <Button size="sm" variant="ghost" onClick={() => void onOpen(row)}>
          {t('common.open')}
        </Button>
        <Button
          size="icon"
          variant="ghost"
          title={t('admin.libraries.configureAi')}
          onClick={() => onConfigureAi(row)}
        >
          <Settings2 size={14} />
        </Button>
        <Button
          size="icon"
          variant="ghost"
          title={t('admin.snapshot.export')}
          onClick={() => onBackup(row)}
        >
          <Download size={14} />
        </Button>
        <Button
          size="icon"
          variant="ghost"
          title={t('admin.snapshot.import')}
          onClick={() => onRestore(row)}
        >
          <Upload size={14} />
        </Button>
        <Button
          size="icon"
          variant="ghost"
          title={t('common.delete')}
          disabled={deleting}
          onClick={() => void onDelete(row)}
        >
          <Trash2 size={14} />
        </Button>
        <Button size="icon" variant="ghost" onClick={() => onDetails(row)}>
          <MoreHorizontal size={15} />
        </Button>
      </div>
    ),
  }
}

function textValues(keys: readonly string[]): Array<string | null> {
  return keys.map((key) => {
    const value = gridFacetValue(key)
    return value === null ? null : String(value)
  })
}

function numberValues(keys: readonly string[]): Array<number | null> {
  return keys.map((key) => {
    const value = gridFacetValue(key)
    return value === null ? null : Number(value)
  })
}

function booleanValues(keys: readonly string[]): Array<boolean | null> {
  return keys.map((key) => {
    const value = gridFacetValue(key)
    return value === null ? null : value === true || value === 'true'
  })
}

/**
 * A cost grid selection -> the canonical cost values the compound filter takes.
 * Each key is decoded and validated; a key that is not a canonical cost object
 * is dropped rather than sent as a guess.
 */
function costValues(keys: readonly string[]): Array<AdminLibraryCostValue | null> {
  return keys.map((key) => {
    const value = gridFacetValue(key)
    if (value === null) return null
    if (typeof value !== 'string') throw new Error('Invalid cost facet value')
    const cost = decodeCostValueKey(value)
    if (!cost) throw new Error('Invalid cost facet value')
    return cost
  })
}

/**
 * Grid filter selection -> the typed per-column filters the list and facet
 * endpoints take. Values are decoded from their typed facet key, so a null
 * bucket stays a real null and a decimal cost keeps every digit.
 */
export function librariesColumnFilters(
  filters: GridState['filters'],
): AdminLibraryColumnFilters | undefined {
  const result: AdminLibraryColumnFilters = {}
  let any = false
  for (const [columnId, keys] of Object.entries(filters)) {
    if (keys.length === 0) continue
    switch (columnId as LibraryGridColumnId) {
      case 'library':
        result.library = textValues(keys)
        break
      case 'workspace':
        result.workspace = textValues(keys)
        break
      case 'documents':
        result.documents = numberValues(keys)
        break
      case 'queue':
        result.queue = numberValues(keys)
        break
      case 'cost':
        result.cost = costValues(keys)
        break
      case 'calls':
        result.calls = numberValues(keys)
        break
      case 'readiness':
        result.readiness = booleanValues(keys)
        break
      case 'lifecycle':
        result.lifecycle = textValues(keys)
        break
      default:
        continue
    }
    any = true
  }
  return any ? result : undefined
}

/** The typed facet slots the server returns for one cost value. */
interface LibraryCostFacetValue {
  decimal?: string | null
  currencyCode?: string | null
  coverage?: string | null
  unpricedCallCount?: number | null
}

/** A facet row from the wire: a scalar value or the typed cost slots. */
export type LibraryFacetRow =
  | { value: string | number | boolean | null; count: number }
  | ({ count: number } & LibraryCostFacetValue)

function isCostFacetRow(row: LibraryFacetRow): row is { count: number } & LibraryCostFacetValue {
  return !('value' in row) || (row as { value?: unknown }).value === undefined
}

/** The canonical cost value of a cost facet row, or null for its NULL bucket. */
function facetCostValue(row: LibraryCostFacetValue): AdminLibraryCostValue | null {
  if (
    row.decimal == null &&
    row.currencyCode == null &&
    row.coverage == null &&
    row.unpricedCallCount == null
  )
    return null
  const value = decodeCostValueKey(
    JSON.stringify({
      amount: row.decimal,
      currencyCode: row.currencyCode,
      coverage: row.coverage,
      unpricedCallCount: row.unpricedCallCount,
    }),
  )
  if (!value) throw new Error('Invalid cost facet value')
  return value
}

/**
 * The one cost key/label of a canonical cost value: the same shared `formatCost`
 * the price cell uses, so a facet label and the cell it filters read alike.
 */
export function costFacetOption(
  value: AdminLibraryCostValue | null,
  count: number,
  t: TFunction,
  locale: Locale,
): GridFacetOption {
  const label =
    value === null
      ? t('common.costUnknown')
      : formatCost(
          costValueToSummary(value),
          t('common.costUnknown'),
          t('documents.unpriced'),
          locale,
        )
  return {
    key: gridFacetKey(value === null ? null : costValueKey(value)),
    value: value === null ? null : costValueKey(value),
    label,
    ...(value === null ? {} : { title: label }),
    count,
  }
}

/** The human label of one facet value, reusing the page's own formatters. */
export function libraryFacetLabel(
  column: LibraryGridColumnId,
  value: string | number | boolean | null,
  t: TFunction,
  locale: Locale,
): string {
  if (value === null) {
    if (column === 'documents' || column === 'calls') return t('common.notAvailable')
    return t('dataGrid.nullValue')
  }
  switch (column) {
    case 'readiness':
      return value === true ? t('admin.libraries.ready') : t('admin.libraries.blocked')
    case 'lifecycle':
      return libraryLifecycleStateLabel(String(value), t)
    case 'documents':
    case 'queue':
    case 'calls':
      return formatNumber(Number(value), locale)
    default:
      return String(value)
  }
}

/** Server facet rows -> typed grid options. The key is derived here, never trusted from the wire. */
export function libraryFacetOptions(
  column: LibraryGridColumnId,
  rows: readonly LibraryFacetRow[],
  t: TFunction,
  locale: Locale,
): GridFacetOption[] {
  if (column === 'cost') {
    return rows
      .filter(isCostFacetRow)
      .map((row) => costFacetOption(facetCostValue(row), row.count, t, locale))
  }
  return rows
    .filter(
      (row): row is { value: string | number | boolean | null; count: number } => 'value' in row,
    )
    .map(({ value, count }) => ({
      key: gridFacetKey(value),
      value,
      label: libraryFacetLabel(column, value, t, locale),
      ...(value === null ? {} : { title: String(value) }),
      count,
    }))
}

/**
 * Options for the active filters of a column, so the toolbar chip reads the
 * same formatted label the menu offered. Counts are unknown here.
 */
export function librarySelectedFacets(
  filters: GridState['filters'],
  t: TFunction,
  locale: Locale,
): Record<string, GridFacetOption[]> {
  const result: Record<string, GridFacetOption[]> = {}
  for (const [columnId, keys] of Object.entries(filters)) {
    if (keys.length === 0) continue
    if (columnId === 'cost') {
      result[columnId] = costValues(keys).map((value) => costFacetOption(value, -1, t, locale))
      continue
    }
    result[columnId] = keys.map((key) => {
      const value = gridFacetValue(key)
      return {
        key,
        value,
        label: libraryFacetLabel(columnId as LibraryGridColumnId, value, t, locale),
        count: -1,
      }
    })
  }
  return result
}

export interface LibraryGridColumnsOptions {
  t: TFunction
  locale: Locale
  /** Open the inspector for a row; wired to the name cell's button. */
  onOpen: (row: AdminLibraryRow) => void
}

/** The one cost cell/label of a row, read from its canonical projection. */
function costCell(row: AdminLibraryRow, t: TFunction, locale: Locale): string {
  const value = rowCostValue(row)
  if (!value) return t('common.costUnknown')
  return formatCost(
    costValueToSummary(value),
    t('common.costUnknown'),
    t('documents.unpriced'),
    locale,
  )
}

/** The eight data columns; actions stay in the page's service column. */
export function libraryGridColumns({
  t,
  locale,
  onOpen,
}: LibraryGridColumnsOptions): GridColumn<AdminLibraryRow>[] {
  return [
    {
      id: 'library',
      label: t('common.library'),
      value: (row) => row.displayName,
      displayText: (row) => row.displayName,
      cell: (row) => (
        <button className="table-link" type="button" onClick={() => onOpen(row)}>
          <strong>{row.displayName}</strong>
          <small>{row.slug}</small>
        </button>
      ),
    },
    {
      id: 'workspace',
      label: t('common.workspace'),
      value: (row) => row.workspace.displayName,
      displayText: (row) => row.workspace.displayName,
    },
    {
      id: 'documents',
      label: t('dashboard.documents'),
      align: 'end',
      value: (row) => row.costSummary?.documentCount ?? null,
      cell: (row) =>
        row.costSummary
          ? formatNumber(row.costSummary.documentCount, locale)
          : t('common.notAvailable'),
      displayText: (row) =>
        row.costSummary
          ? formatNumber(row.costSummary.documentCount, locale)
          : t('common.notAvailable'),
    },
    {
      id: 'queue',
      label: t('admin.libraries.queued'),
      align: 'end',
      value: (row) => row.queuedJobCount,
      cell: (row) => <LibraryQueueLink row={row} />,
      displayText: (row) => String(row.queuedJobCount),
    },
    {
      id: 'cost',
      label: t('admin.libraries.cost'),
      align: 'end',
      value: (row) => rowCostValue(row)?.amount ?? null,
      cell: (row) => costCell(row, t, locale),
      displayText: (row) => costCell(row, t, locale),
    },
    {
      id: 'calls',
      label: t('admin.libraries.calls'),
      align: 'end',
      value: (row) => row.costSummary?.providerCallCount ?? null,
      cell: (row) =>
        row.costSummary
          ? formatNumber(row.costSummary.providerCallCount, locale)
          : t('common.notAvailable'),
      displayText: (row) =>
        row.costSummary
          ? formatNumber(row.costSummary.providerCallCount, locale)
          : t('common.notAvailable'),
    },
    {
      id: 'readiness',
      label: t('admin.libraries.readiness'),
      value: (row) => row.readiness.ready,
      displayText: (row) =>
        row.readiness.ready ? t('admin.libraries.ready') : t('admin.libraries.blocked'),
      cell: (row) => (
        <div className="cell-title">
          <Badge tone={row.readiness.ready ? 'ready' : 'failed'}>
            {row.readiness.ready ? t('admin.libraries.ready') : t('admin.libraries.blocked')}
          </Badge>
          {!row.readiness.ready && row.readiness.missingBindingPurposes.length > 0 && (
            <MissingBindingPurposeLabels purposes={row.readiness.missingBindingPurposes} />
          )}
        </div>
      ),
    },
    {
      id: 'lifecycle',
      label: t('admin.libraries.lifecycle'),
      value: (row) => row.lifecycleState,
      displayText: (row) => libraryLifecycleStateLabel(row.lifecycleState, t),
      cell: (row) => (
        <Badge tone={row.lifecycleState === 'active' ? 'ready' : 'neutral'}>
          {libraryLifecycleStateLabel(row.lifecycleState, t)}
        </Badge>
      ),
    },
  ]
}
