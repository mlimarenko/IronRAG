import { Download } from 'lucide-react'
import { useCallback, useMemo, useState } from 'react'
import type { SetStateAction } from 'react'
import { useTranslation } from 'react-i18next'

import { DataGrid, type GridState } from '@/shared/data-grid'
import { useApp } from '@/shared/contexts/app-context'
import { useQueuedSearchParams } from '@/shared/hooks/useQueuedSearchParams'
import { errorMessage } from '@/shared/lib/errorMessage'
import { Button, PageHeader, Panel } from '@/shared/ui'

import {
  useAuditEvents,
  useAuditFacets,
  useAuditSelectedValues,
  type AuditScope,
} from './audit/api'
import {
  auditColumnFilters,
  auditGridColumns,
  auditSelectedFacets,
  parseAuditFilters,
  parseAuditSort,
  serializeAuditFilters,
  type AuditEvent,
} from './audit/model'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

const PAGE_SIZE_OPTIONS = [25, 50, 100] as const
const DEFAULT_PAGE_SIZE = 50

function auditGridState(params: URLSearchParams): GridState {
  const rawSize = Number(params.get('pageSize'))
  return {
    search: params.get('q') ?? '',
    sort: parseAuditSort(params.get('sort')),
    filters: parseAuditFilters(params.get('filters')),
    page: 1,
    pageSize: (PAGE_SIZE_OPTIONS as readonly number[]).includes(rawSize)
      ? rawSize
      : DEFAULT_PAGE_SIZE,
  }
}

function exportEvents(events: readonly AuditEvent[]) {
  const headings = ['createdAt', 'actionKind', 'actor', 'surfaceKind', 'resultKind', 'message']
  const escape = (value: string) => `"${value.replaceAll('"', '""')}"`
  const rows = events.map((event) =>
    [
      event.createdAt,
      event.actionKind,
      event.actor,
      event.surfaceKind,
      event.resultKind,
      event.redactedMessage,
    ]
      .map(escape)
      .join(','),
  )
  const blob = new Blob([[headings.join(','), ...rows].join('\n')], { type: 'text/csv' })
  const href = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = href
  anchor.download = 'audit-events.csv'
  anchor.click()
  URL.revokeObjectURL(href)
}

export default function AuditAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const { libraries, locale, workspaces } = useApp()
  const [params, setParams] = useQueuedSearchParams()
  const [cursorStack, setCursorStack] = useState<Array<string | null>>([null])

  const workspaceId = params.get('workspaceId') ?? ''
  const libraryId = params.get('libraryId') ?? ''
  const internal = params.get('internal') === '1'
  const includeAssistant = params.get('assistant') !== '0'

  const gridState = useMemo(() => auditGridState(params), [params])
  const pageSize = gridState.pageSize

  const scope = useMemo<AuditScope>(
    () => ({
      workspaceId,
      libraryId,
      internal,
      includeAssistant,
      filters: auditColumnFilters(gridState.filters),
      search: gridState.search,
      sort: gridState.sort,
      limit: pageSize,
    }),
    [
      gridState.filters,
      gridState.search,
      gridState.sort,
      includeAssistant,
      internal,
      libraryId,
      pageSize,
      workspaceId,
    ],
  )

  const cursor = cursorStack[cursorStack.length - 1] ?? null
  const auditQuery = useAuditEvents(scope, cursor, t, locale)
  const { serverFacets } = useAuditFacets(scope, true, locale, t)
  const { pending: selectedPending, resolveLabel } = useAuditSelectedValues(
    scope,
    gridState.filters,
    locale,
    t,
  )

  const visibleLibraries = workspaceId
    ? libraries.filter((library) => library.workspaceId === workspaceId)
    : libraries
  const items = auditQuery.data?.items ?? []
  const total = auditQuery.data?.total ?? null
  const pending = auditQuery.isLoading || auditQuery.isPlaceholderData || selectedPending

  const resetCursor = useCallback(() => setCursorStack([null]), [])
  const applyGridState = useCallback(
    (update: SetStateAction<GridState>) => {
      setParams((nextParams) => {
        const next = typeof update === 'function' ? update(auditGridState(nextParams)) : update
        const put = (key: string, value: string | null) => {
          if (value == null || value === '') nextParams.delete(key)
          else nextParams.set(key, value)
        }
        put('q', next.search)
        put('sort', next.sort ? `${next.sort.columnId}:${next.sort.direction}` : null)
        put('filters', serializeAuditFilters(next.filters))
        put('pageSize', next.pageSize === DEFAULT_PAGE_SIZE ? null : String(next.pageSize))
        return nextParams
      })
      resetCursor()
    },
    [resetCursor, setParams],
  )
  const updateParam = useCallback(
    (key: string, value: string) => {
      setParams((next) => {
        if (value) next.set(key, value)
        else next.delete(key)
        return next
      })
      resetCursor()
    },
    [resetCursor, setParams],
  )
  const updateWorkspace = useCallback(
    (value: string) => {
      setParams((next) => {
        if (value) next.set('workspaceId', value)
        else next.delete('workspaceId')
        next.delete('libraryId')
        return next
      })
      resetCursor()
    },
    [resetCursor, setParams],
  )

  const columns = useMemo(
    () => auditGridColumns({ locale, t, showInternal: internal, showAssistant: includeAssistant }),
    [internal, locale, t, includeAssistant],
  )
  const selectedFacets = useMemo(
    () => auditSelectedFacets(gridState.filters, t, locale, resolveLabel),
    [gridState.filters, locale, resolveLabel, t],
  )
  const canNext = Boolean(auditQuery.data?.nextCursor)
  const canPrevious = cursorStack.length > 1
  const shown = useMemo(() => {
    if (items.length === 0) return null
    const from = (cursorStack.length - 1) * pageSize + 1
    return { from, to: from + items.length - 1 }
  }, [cursorStack.length, items.length, pageSize])

  const nextPage = () => {
    if (pending) return
    const next = auditQuery.data?.nextCursor
    if (next) setCursorStack((current) => [...current, next])
  }
  const previousPage = () => {
    if (pending) return
    setCursorStack((current) => (current.length > 1 ? current.slice(0, -1) : current))
  }

  return (
    <div className="admin-ops-page">
      <PageHeader
        actions={
          <Button disabled={pending || items.length === 0} onClick={() => exportEvents(items)}>
            <Download size={15} />
            {t('admin.exportCurrentPage', { count: items.length })}
          </Button>
        }
        description={t('admin.auditSubtitle')}
        eyebrow={adminBreadcrumb}
        title={t('admin.auditTitle')}
      />
      <Panel>
        <DataGrid<AuditEvent>
          mode="remote"
          rows={items}
          rowId={(event) => event.id}
          columns={columns}
          state={gridState}
          onStateChange={applyGridState}
          ariaLabel={t('admin.auditTitle')}
          searchPlaceholder={t('admin.auditSearchPlaceholder')}
          emptyText={t('admin.noAuditEvents')}
          noMatchText={t('admin.noAuditEvents')}
          {...(auditQuery.error
            ? {
                errorText: errorMessage(auditQuery.error, t('admin.loadAuditEventsFailed')),
                onRetry: () => void auditQuery.refetch(),
              }
            : {})}
          pending={pending}
          total={total}
          serverFacets={serverFacets}
          facets={selectedFacets}
          pagination={{
            canPrevious,
            canNext,
            onPrevious: previousPage,
            onNext: nextPage,
            shown,
          }}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          toolbarExtras={
            <>
              <select
                aria-label={t('admin.auditWorkspaceAll')}
                className="input compact-input"
                onChange={(event) => updateWorkspace(event.target.value)}
                value={workspaceId}
              >
                <option value="">{t('admin.auditWorkspaceAll')}</option>
                {workspaces.map((workspace) => (
                  <option key={workspace.id} value={workspace.id}>
                    {workspace.name}
                  </option>
                ))}
              </select>
              <select
                aria-label={t('common.library')}
                className="input compact-input"
                onChange={(event) => updateParam('libraryId', event.target.value)}
                value={libraryId}
              >
                <option value="">{t('admin.auditLibraryAll')}</option>
                {visibleLibraries.map((library) => (
                  <option key={library.id} value={library.id}>
                    {library.name}
                  </option>
                ))}
              </select>
              <label className="switch">
                <input
                  checked={internal}
                  onChange={(event) => updateParam('internal', event.target.checked ? '1' : '')}
                  type="checkbox"
                />
                <span />
                {t('admin.showInternal')}
              </label>
              <label className="switch">
                <input
                  checked={includeAssistant}
                  onChange={(event) => updateParam('assistant', event.target.checked ? '' : '0')}
                  type="checkbox"
                />
                <span />
                {t('admin.auditIncludeAssistant')}
              </label>
            </>
          }
        />
      </Panel>
    </div>
  )
}
