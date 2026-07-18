import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import {
  AlertTriangle,
  Boxes,
  Building2,
  CheckCircle2,
  Search,
  Server,
  XCircle,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { adminApi, queries } from '@/shared/api'
import { DataState } from '@/shared/components/DataState'
import { FilterSelect } from '@/shared/components/FilterSelect'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import { StatusBadge, type StatusTone } from '@/shared/components/StatusBadge'
import { TablePaginationFooter } from '@/shared/components/TablePaginationFooter'
import { Input } from '@/shared/components/ui/input'
import { SelectItem } from '@/shared/components/ui/select'
import { mapAuditPage } from '@/features/admin/model/adminAdapter'
import { errorMessage } from '@/shared/lib/errorMessage'
import {
  isStorageRecord,
  parseNumberOption,
  parseStringOption,
  useTableState,
} from '@/shared/hooks/useTableState'
import type { AuditEvent, AuditEventPage } from '@/shared/types'
import type { ListAuditEventsData } from '@/shared/api/generated'

const AUDIT_PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const
const AUDIT_SURFACE_OPTIONS = ['all', 'rest', 'mcp', 'worker', 'bootstrap'] as const
const AUDIT_RESULT_OPTIONS = ['all', 'succeeded', 'rejected', 'failed'] as const

type AuditResultFilter = (typeof AUDIT_RESULT_OPTIONS)[number]
type AuditSurfaceFilter = (typeof AUDIT_SURFACE_OPTIONS)[number]
type AuditPageSize = (typeof AUDIT_PAGE_SIZE_OPTIONS)[number]

type AuditTableState = {
  pageSize: AuditPageSize
  resultFilter: AuditResultFilter
  surfaceFilter: AuditSurfaceFilter
}

const DEFAULT_AUDIT_TABLE_STATE: AuditTableState = {
  pageSize: AUDIT_PAGE_SIZE_OPTIONS[0],
  resultFilter: 'all',
  surfaceFilter: 'all',
}

function getAuditResultTone(resultKind: AuditEvent['resultKind']): StatusTone {
  if (resultKind === 'failed') return 'failed'
  if (resultKind === 'rejected') return 'warning'
  return 'ready'
}

function getAuditResultClassName(resultKind: AuditEvent['resultKind']): string {
  if (resultKind === 'failed') return 'text-status-failed'
  if (resultKind === 'rejected') return 'text-status-warning'
  return 'text-status-ready'
}

function getAuditResultIcon(resultKind: AuditEvent['resultKind']) {
  if (resultKind === 'failed') return XCircle
  if (resultKind === 'rejected') return AlertTriangle
  return CheckCircle2
}

function humanizeAuditSurface(surfaceKind: string, t: TFunction): string {
  switch (surfaceKind) {
    case 'mcp':
    case 'worker':
    case 'bootstrap':
    case 'rest':
      return t(`admin.auditSurfaceLabels.${surfaceKind}`)
    default:
      return surfaceKind
  }
}

function humanizeAuditResult(resultKind: AuditEvent['resultKind'], t: TFunction): string {
  return t(`admin.auditResultLabels.${resultKind}`)
}

function formatAuditAssistantModels(event: AuditEvent, t: TFunction): string {
  const assistantCall = event.assistantCall
  if (!assistantCall || assistantCall.models.length === 0) {
    return t('admin.auditAssistantNoModel')
  }
  return assistantCall.models.map((model) => `${model.providerKind}:${model.modelName}`).join(', ')
}

function formatAuditAssistantCost(event: AuditEvent, t: TFunction): string {
  const assistantCall = event.assistantCall
  if (assistantCall?.totalCost == null) {
    return t('admin.auditAssistantCostUnavailable')
  }
  return `$${Number(assistantCall.totalCost).toFixed(4)}`
}

export default function AdminAuditPage() {
  const { t } = useTranslation()

  const [auditSearch, setAuditSearch] = useState('')
  const [auditPage, setAuditPage] = useState(1)
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState('all')
  const [selectedLibraryId, setSelectedLibraryId] = useState('all')

  const [auditTableState, setAuditTableState] = useTableState<AuditTableState>({
    tableId: 'admin.audit',
    defaultValue: DEFAULT_AUDIT_TABLE_STATE,
    parse: (raw) => {
      const record = isStorageRecord(raw) ? raw : {}
      return {
        pageSize: parseNumberOption(
          record.pageSize,
          AUDIT_PAGE_SIZE_OPTIONS,
          DEFAULT_AUDIT_TABLE_STATE.pageSize,
        ),
        resultFilter: parseStringOption(
          record.resultFilter,
          AUDIT_RESULT_OPTIONS,
          DEFAULT_AUDIT_TABLE_STATE.resultFilter,
        ),
        surfaceFilter: parseStringOption(
          record.surfaceFilter,
          AUDIT_SURFACE_OPTIONS,
          DEFAULT_AUDIT_TABLE_STATE.surfaceFilter,
        ),
      }
    },
  })

  const {
    pageSize: auditPageSize,
    resultFilter: auditResultFilter,
    surfaceFilter: auditSurfaceFilter,
  } = auditTableState

  const workspacesQuery = useQuery({
    ...queries.listCatalogWorkspacesOptions(),
  })
  const workspaces = workspacesQuery.data ?? []

  const wsActive = selectedWorkspaceId !== 'all'
  const libActive = selectedLibraryId !== 'all'

  const librariesQuery = useQuery({
    ...queries.listCatalogLibrariesOptions({
      path: { workspaceId: selectedWorkspaceId },
    }),
    enabled: wsActive,
  })
  const libraries = librariesQuery.data ?? []
  const librariesCount = libraries.length

  const auditScopeQuery: Partial<NonNullable<ListAuditEventsData['query']>> = {}
  if (libActive) {
    auditScopeQuery.libraryId = selectedLibraryId
  } else if (wsActive) {
    auditScopeQuery.workspaceId = selectedWorkspaceId
  }
  const auditQueryParams: NonNullable<ListAuditEventsData['query']> = {
    ...auditScopeQuery,
    ...(auditSearch ? { search: auditSearch } : {}),
    ...(auditSurfaceFilter === 'all' ? {} : { surfaceKind: auditSurfaceFilter }),
    ...(auditResultFilter === 'all' ? {} : { resultKind: auditResultFilter }),
    limit: auditPageSize,
    offset: (auditPage - 1) * auditPageSize,
    includeAssistant: true,
  }

  const auditQueryOptions = queries.listAuditEventsOptions({
    query: auditQueryParams,
  })

  const auditQuery = useQuery({
    ...auditQueryOptions,
    queryFn: async () => {
      const firstPage = await adminApi.listAuditEvents(auditQueryParams)
      const mappedFirstPage = mapAuditPage(firstPage)
      const totalPages = Math.max(1, Math.ceil(mappedFirstPage.total / auditPageSize))
      if (mappedFirstPage.total > 0 && auditPage > totalPages) {
        return adminApi.listAuditEvents({
          ...auditQueryParams,
          offset: (totalPages - 1) * auditPageSize,
        })
      }
      return firstPage
    },
    enabled: true,
  })

  const audit = useMemo<AuditEventPage>(() => {
    if (!auditQuery.data) {
      return { items: [], total: 0, limit: auditPageSize, offset: 0 }
    }
    return mapAuditPage(auditQuery.data)
  }, [auditQuery.data, auditPageSize])

  const auditLoading = auditQuery.isLoading

  const auditTotalPages = Math.max(1, Math.ceil(audit.total / auditPageSize))
  const visibleAuditPage = audit.total === 0 ? 1 : Math.floor(audit.offset / auditPageSize) + 1
  const auditFrom = audit.total === 0 ? 0 : audit.offset + 1
  const auditTo = audit.total === 0 ? 0 : Math.min(audit.total, auditFrom + audit.items.length - 1)

  return (
    <PageShell
      header={<PageHeader title={t('admin.nav.audit')} description={t('admin.nav.auditDesc')} />}
      bodyClassName="flex flex-col overflow-hidden"
    >
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden animate-fade-in">
        {/* ── Filters band (full-bleed, canonical list-page toolbar) ── */}
        <div className="shrink-0 flex flex-wrap items-center gap-3 border-b bg-surface-sunken/50 px-6 py-3">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
            <Input
              className="h-9 pl-9 w-48 text-xs"
              placeholder={t('admin.auditSearchPlaceholder')}
              value={auditSearch}
              onChange={(e) => {
                setAuditSearch(e.target.value)
                setAuditPage(1)
              }}
            />
          </div>
          <FilterSelect
            value={selectedWorkspaceId}
            onValueChange={(v) => {
              setSelectedWorkspaceId(v)
              setSelectedLibraryId('all')
              setAuditPage(1)
            }}
            icon={<Building2 />}
            className="w-[180px]"
          >
            <SelectItem value="all">
              {t('admin.queueAllWorkspaces', { count: workspaces.length })}
            </SelectItem>
            {workspaces.map((ws) => (
              <SelectItem key={ws.id} value={ws.id}>
                {ws.displayName}
              </SelectItem>
            ))}
          </FilterSelect>
          <FilterSelect
            value={selectedLibraryId}
            onValueChange={(v) => {
              setSelectedLibraryId(v)
              setAuditPage(1)
            }}
            disabled={!wsActive}
            icon={<Boxes />}
            className="w-[180px]"
          >
            <SelectItem value="all">
              {t('admin.queueAllLibraries', { count: librariesCount })}
            </SelectItem>
            {libraries.map((lib) => (
              <SelectItem key={lib.id} value={lib.id}>
                {lib.displayName}
              </SelectItem>
            ))}
          </FilterSelect>
          <FilterSelect
            value={auditResultFilter}
            onValueChange={(v) => {
              setAuditTableState((prev) => ({
                ...prev,
                resultFilter: v as AuditResultFilter,
              }))
              setAuditPage(1)
            }}
            icon={<CheckCircle2 />}
            className="w-[180px]"
          >
            {AUDIT_RESULT_OPTIONS.map((o) => (
              <SelectItem key={o} value={o}>
                {o === 'all' ? t('admin.auditResultAll') : humanizeAuditResult(o, t)}
              </SelectItem>
            ))}
          </FilterSelect>
          <FilterSelect
            value={auditSurfaceFilter}
            onValueChange={(v) => {
              setAuditTableState((prev) => ({
                ...prev,
                surfaceFilter: v as AuditSurfaceFilter,
              }))
              setAuditPage(1)
            }}
            icon={<Server />}
            className="w-[180px]"
          >
            {AUDIT_SURFACE_OPTIONS.map((o) => (
              <SelectItem key={o} value={o}>
                {o === 'all' ? t('admin.auditSurfaceAll') : humanizeAuditSurface(o, t)}
              </SelectItem>
            ))}
          </FilterSelect>
        </div>

        {/* ── Audit table ── */}
        <div className="flex-1 min-h-0 flex flex-col">
          <DataState
            query={{
              isLoading: auditLoading,
              error: auditQuery.error
                ? errorMessage(auditQuery.error, t('admin.loadAuditEventsFailed'))
                : null,
              data: audit,
            }}
            emptyCheck={(auditData) => auditData.items.length === 0}
            emptyRender={<WorkbenchEmptyState title={t('admin.noAuditEvents')} />}
          >
            {(auditData) => (
              <>
                <div className="flex-1 min-h-0 overflow-auto">
                  <div className="space-y-3 p-3 xl:hidden">
                    {auditData.items.map((evt) => {
                      const ResultIcon = getAuditResultIcon(evt.resultKind)
                      const assistantModels = evt.assistantCall
                        ? formatAuditAssistantModels(evt, t)
                        : ''
                      const assistantCost = evt.assistantCall
                        ? formatAuditAssistantCost(evt, t)
                        : ''
                      return (
                        <article key={evt.id} className="workbench-surface p-4">
                          <div className="flex items-start justify-between gap-3">
                            <div className="flex min-w-0 items-start gap-2">
                              <div
                                className={`mt-0.5 shrink-0 ${getAuditResultClassName(evt.resultKind)}`}
                              >
                                <ResultIcon className="h-3.5 w-3.5" />
                              </div>
                              <div className="min-w-0">
                                <div className="truncate text-sm font-semibold" title={evt.message}>
                                  {evt.message.split(' | ')[0]}
                                </div>
                                <div className="mt-1 text-xs text-muted-foreground">
                                  {evt.actor}
                                </div>
                              </div>
                            </div>
                            <StatusBadge
                              tone={getAuditResultTone(evt.resultKind)}
                              className="shrink-0"
                            >
                              {humanizeAuditResult(evt.resultKind, t)}
                            </StatusBadge>
                          </div>
                          <div className="mt-3 flex flex-wrap items-center gap-2 text-xs">
                            <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 section-label">
                              {humanizeAuditSurface(evt.surfaceKind, t)}
                            </span>
                            <span className="text-muted-foreground tabular-nums">
                              {new Date(evt.timestamp).toLocaleString()}
                            </span>
                          </div>
                          <div className="mt-2 truncate text-xs text-muted-foreground">
                            {evt.assistantCall ? (
                              <span title={assistantModels}>
                                {t('admin.auditAssistantMeta', {
                                  cost: assistantCost,
                                  count: evt.assistantCall.providerCallCount,
                                })}
                              </span>
                            ) : (
                              <span title={evt.subjectSummary ?? undefined}>
                                {evt.subjectSummary || '—'}
                              </span>
                            )}
                          </div>
                        </article>
                      )
                    })}
                  </div>
                  <table className="hidden w-full min-w-[1100px] table-fixed text-sm xl:table">
                    <colgroup>
                      <col className="w-10" />
                      <col />
                      <col className="w-40" />
                      <col className="w-28" />
                      <col className="w-44" />
                      <col className="w-56" />
                      <col className="w-48" />
                    </colgroup>
                    <thead className="sticky top-0 z-10 bg-card">
                      <tr className="border-b text-left">
                        <th className="px-4 py-3" />
                        <th className="px-4 py-3 section-label">{t('admin.auditAction')}</th>
                        <th className="px-4 py-3 section-label">{t('admin.auditActor')}</th>
                        <th className="px-4 py-3 section-label">{t('admin.auditSurface')}</th>
                        <th className="px-4 py-3 section-label">{t('admin.auditTime')}</th>
                        <th className="px-4 py-3 section-label">{t('admin.auditDetails')}</th>
                        <th className="px-4 py-3 section-label">{t('admin.auditResult')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {auditData.items.map((evt) => {
                        const ResultIcon = getAuditResultIcon(evt.resultKind)
                        const assistantModels = evt.assistantCall
                          ? formatAuditAssistantModels(evt, t)
                          : ''
                        const assistantCost = evt.assistantCall
                          ? formatAuditAssistantCost(evt, t)
                          : ''
                        return (
                          <tr
                            key={evt.id}
                            className="border-b border-border/50 hover:bg-accent/30 transition-colors"
                          >
                            <td className="px-4 py-3">
                              <div className={getAuditResultClassName(evt.resultKind)}>
                                <ResultIcon className="h-3.5 w-3.5" />
                              </div>
                            </td>
                            <td className="px-4 py-3">
                              <div
                                className="font-semibold text-xs leading-tight truncate max-w-md"
                                title={evt.message}
                              >
                                {evt.message.split(' | ')[0]}
                              </div>
                            </td>
                            <td className="px-4 py-3 text-xs text-muted-foreground font-medium whitespace-nowrap">
                              {evt.actor}
                            </td>
                            <td className="px-4 py-3 text-xs whitespace-nowrap">
                              <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 section-label">
                                {humanizeAuditSurface(evt.surfaceKind, t)}
                              </span>
                            </td>
                            <td className="px-4 py-3 text-xs text-muted-foreground tabular-nums whitespace-nowrap">
                              {new Date(evt.timestamp).toLocaleString()}
                            </td>
                            <td className="px-4 py-3 text-xs text-muted-foreground max-w-64">
                              {evt.assistantCall ? (
                                <div className="truncate" title={assistantModels}>
                                  {t('admin.auditAssistantMeta', {
                                    cost: assistantCost,
                                    count: evt.assistantCall.providerCallCount,
                                  })}
                                </div>
                              ) : (
                                <div className="truncate" title={evt.subjectSummary ?? undefined}>
                                  {evt.subjectSummary || '—'}
                                </div>
                              )}
                            </td>
                            <td className="px-4 py-3">
                              <StatusBadge tone={getAuditResultTone(evt.resultKind)}>
                                {humanizeAuditResult(evt.resultKind, t)}
                              </StatusBadge>
                            </td>
                          </tr>
                        )
                      })}
                    </tbody>
                  </table>
                </div>

                {/* ── Pagination footer (shared canonical footer) ── */}
                <TablePaginationFooter
                  canGoPrevious={visibleAuditPage > 1}
                  canGoNext={visibleAuditPage < auditTotalPages}
                  currentPageNumber={visibleAuditPage}
                  goToPreviousPage={() => setAuditPage(Math.max(1, visibleAuditPage - 1))}
                  goToNextPage={() => setAuditPage(Math.min(auditTotalPages, visibleAuditPage + 1))}
                  goToPage={(target) => setAuditPage(target)}
                  pageSize={auditPageSize}
                  pageSizeLabel={t('documents.pageSize')}
                  pageSizeOptions={AUDIT_PAGE_SIZE_OPTIONS}
                  previousLabel={t('admin.previous')}
                  nextLabel={t('admin.next')}
                  onPageSizeChange={(size) => {
                    setAuditTableState((prev) => ({ ...prev, pageSize: size }))
                    setAuditPage(1)
                  }}
                  summary={t('admin.auditSummary', {
                    from: auditFrom,
                    to: auditTo,
                    total: audit.total,
                  })}
                  totalPages={auditTotalPages}
                />
              </>
            )}
          </DataState>
        </div>
      </div>
    </PageShell>
  )
}
