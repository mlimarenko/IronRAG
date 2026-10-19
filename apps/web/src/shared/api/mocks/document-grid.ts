import type {
  ContentDocumentListItem,
  DocumentColumnFilters,
  DocumentFacetColumn,
  DocumentFacetsResponse,
  DocumentListPageResponse,
  DocumentSearchRanges,
} from '../generated'

// Browser fixtures simulate the remote contract, never a client-page fallback.
type GridScalar = string | number | null

const columnKeys: Record<DocumentFacetColumn, keyof DocumentColumnFilters> = {
  uploaded_at: 'uploadedAt',
  file_name: 'fileName',
  file_type: 'fileType',
  file_size: 'fileSize',
  status: 'status',
  cost: 'cost',
  pipeline_time: 'pipelineTime',
  finished_at: 'finishedAt',
}

function valueOf(
  row: ContentDocumentListItem,
  column: DocumentFacetColumn,
  snapshot: string,
): GridScalar {
  if (column === 'pipeline_time') {
    // Same instant resolution as the React duration adapter: a processing job
    // measures against the snapshot, a finished one against its finish time,
    // a queued one against the snapshot, and an unstarted one has none.
    let end: string | null
    if (row.status === 'processing') end = snapshot
    else if (row.processingFinishedAt != null) end = row.processingFinishedAt
    else if (row.status === 'queued') end = snapshot
    else end = null
    return row.processingStartedAt && end
      ? Math.max(0, Date.parse(end) - Date.parse(row.processingStartedAt))
      : null
  }
  if (column === 'finished_at') return row.processingFinishedAt ?? null
  if (column === 'uploaded_at') return row.uploadedAt
  if (column === 'file_name') return row.fileName
  if (column === 'file_type') return row.fileType ?? null
  if (column === 'file_size') return row.fileSize ?? null
  if (column === 'status') return row.status
  return row.cost ?? null
}

function filteredRows(
  rows: ContentDocumentListItem[],
  query: URLSearchParams,
  snapshot: string,
  excluded?: DocumentFacetColumn,
) {
  const filters = JSON.parse(query.get('filters') ?? '{}') as DocumentColumnFilters
  const statuses = query.get('status')?.split(',')
  const ids = query.get('ids')?.split(',')
  const patterns = [query.get('search'), query.get('normalizedSearch')]
    .filter((pattern): pattern is string => Boolean(pattern?.trim()))
    .map((pattern) => pattern.toLowerCase())
  const searchStatuses = query.get('searchStatus')?.split(',') ?? []
  const ranges = JSON.parse(query.get('searchRanges') ?? '{}') as DocumentSearchRanges
  return rows.filter((row) => {
    if (statuses && !statuses.includes(row.status)) return false
    if (ids && !ids.includes(row.id)) return false
    if (query.get('indexing') === 'graph_ready' && row.readiness !== 'graph_ready') return false
    if (query.get('indexing') === 'graph_gap' && row.readiness === 'graph_ready') return false
    const columns = Object.keys(columnKeys) as DocumentFacetColumn[]
    if (
      !columns.every((column) => {
        const selected = filters[columnKeys[column]] as Array<GridScalar> | null | undefined
        return (
          column === excluded ||
          !selected?.length ||
          selected.includes(valueOf(row, column, snapshot))
        )
      })
    )
      return false
    if (!patterns.length && !searchStatuses.length && !Object.keys(ranges).length) return true
    const size = valueOf(row, 'file_size', snapshot)
    const duration = valueOf(row, 'pipeline_time', snapshot)
    const rangeMatch =
      (ranges.fileSize &&
        typeof size === 'number' &&
        size >= ranges.fileSize.min &&
        size < ranges.fileSize.max) ||
      (ranges.pipelineTime &&
        typeof duration === 'number' &&
        duration >= ranges.pipelineTime.min &&
        duration < ranges.pipelineTime.max) ||
      (ranges.date &&
        [row.uploadedAt, row.processingFinishedAt].some(
          (value) =>
            value &&
            Date.parse(value) >= Date.parse(ranges.date!.min) &&
            Date.parse(value) < Date.parse(ranges.date!.max),
        ))
    return (
      rangeMatch ||
      searchStatuses.includes(row.status) ||
      patterns.some((pattern) =>
        [row.externalKey, ...columns.map((column) => valueOf(row, column, snapshot))].some(
          (value) => value !== null && String(value).toLowerCase().includes(pattern),
        ),
      )
    )
  })
}

function compareValues(left: GridScalar, right: GridScalar, decimal = false) {
  if (left === null) return right === null ? 0 : 1
  if (right === null) return -1
  if (typeof left === 'number' && typeof right === 'number') return left - right
  if (decimal) {
    const scale = Math.max(
      String(left).split('.')[1]?.length ?? 0,
      String(right).split('.')[1]?.length ?? 0,
    )
    const scaled = (value: string | number) => {
      const [whole = '0', fraction = ''] = String(value).split('.')
      return BigInt(whole + fraction.padEnd(scale, '0'))
    }
    const a = scaled(left)
    const b = scaled(right)
    if (a < b) return -1
    if (a > b) return 1
    return 0
  }
  return String(left).localeCompare(String(right), 'en', { numeric: true })
}

export function documentGridPage(
  rows: ContentDocumentListItem[],
  query: URLSearchParams,
): DocumentListPageResponse {
  const snapshotAt = query.get('snapshotAt') ?? '2026-10-01T12:00:00.000Z'
  const scoped = filteredRows(rows, query, snapshotAt)
  const column = (query.get('sortBy') ?? 'uploaded_at') as DocumentFacetColumn
  const descending = query.get('sortOrder') !== 'asc'
  const ordered = [...scoped].sort((left, right) => {
    const a = valueOf(left, column, snapshotAt)
    const b = valueOf(right, column, snapshotAt)
    const comparison = compareValues(a, b, column === 'cost')
    const primary = descending && a !== null && b !== null ? -comparison : comparison
    return primary || right.id.localeCompare(left.id)
  })
  const offset = Number(query.get('cursor') ?? 0)
  const limit = Number(query.get('limit') ?? 25)
  const items = ordered.slice(offset, offset + limit)
  const statusCounts = {
    canceled: 0,
    empty: 0,
    failed: 0,
    processing: 0,
    queued: 0,
    ready: 0,
    total: scoped.length,
  }
  for (const row of scoped) statusCounts[row.status] += 1
  return {
    items,
    snapshotAt,
    statusCounts,
    totalCount: scoped.length,
    nextCursor: offset + limit < ordered.length ? String(offset + limit) : null,
    costRollupFreshness: 'current',
    scopePending: false,
  }
}

export function documentGridFacets(
  rows: ContentDocumentListItem[],
  query: URLSearchParams,
): DocumentFacetsResponse {
  const column = query.get('facetColumn') as DocumentFacetColumn
  const snapshotAt = query.get('snapshotAt') ?? '2026-10-01T12:00:00.000Z'
  const grouped = new Map<GridScalar, number>()
  const facetRanges = JSON.parse(query.get('facetSearchRanges') ?? '{}') as DocumentSearchRanges
  const facetStatuses = query.get('facetSearchStatus')?.split(',') ?? []
  for (const row of filteredRows(rows, query, snapshotAt, column)) {
    const value = valueOf(row, column, snapshotAt)
    const search = query.get('facetSearch')?.toLowerCase()
    const range = column === 'file_size' ? facetRanges.fileSize : facetRanges.pipelineTime
    const numberMatches =
      (column === 'file_size' || column === 'pipeline_time') &&
      range &&
      typeof value === 'number' &&
      value >= range.min &&
      value < range.max
    const dateMatches =
      facetRanges.date &&
      (column === 'uploaded_at' || column === 'finished_at') &&
      typeof value === 'string' &&
      Date.parse(value) >= Date.parse(facetRanges.date.min) &&
      Date.parse(value) < Date.parse(facetRanges.date.max)
    const statusMatches =
      column === 'status' && typeof value === 'string' && facetStatuses.includes(value)
    if (
      (search || Object.keys(facetRanges).length || facetStatuses.length) &&
      !(
        numberMatches ||
        dateMatches ||
        statusMatches ||
        (search && value !== null && String(value).toLowerCase().includes(search))
      )
    )
      continue
    grouped.set(value, (grouped.get(value) ?? 0) + 1)
  }
  const values = [...grouped]
    .map(([value, count]) => ({ value, count }))
    .sort((left, right) => compareValues(left.value, right.value, column === 'cost'))
  const offset = Number(query.get('facetCursor') ?? 0)
  const limit = Number(query.get('facetLimit') ?? 50)
  return {
    column,
    snapshotAt,
    values: values.slice(offset, offset + limit),
    nextCursor: offset + limit < values.length ? String(offset + limit) : null,
    costRollupFreshness: 'current',
    scopePending: false,
  }
}
