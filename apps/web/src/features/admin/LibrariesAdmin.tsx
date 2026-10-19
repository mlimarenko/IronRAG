import { useCallback, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

import type { AdminLibrarySortKey, AdminLibrarySortOrder } from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { DataGrid, type GridState } from '@/shared/data-grid'
import { formatCost } from '@/shared/format'
import { errorMessage } from '@/shared/lib/errorMessage'
import { mapCatalogLibrary } from '@/shared/lib/library'
import { PageHeader, Panel } from '@/shared/ui'
import {
  useAdminLibrariesQuery,
  useDebouncedValue,
  useLibraryAdminMutations,
  useLibraryFacets,
} from '@/features/admin/libraries/api/useLibrariesAdmin'
import { LibraryInspector } from '@/features/admin/libraries/components/LibraryInspector'
import {
  SnapshotExportDialog,
  SnapshotRestoreDialog,
} from '@/features/admin/libraries/components/SnapshotDialogs'
import {
  type AdminLibraryRow,
  type LibrarySettingsDraft,
} from '@/features/admin/libraries/model/librariesAdapter'
import {
  librariesColumnFilters,
  libraryActionsColumn,
  libraryGridColumns,
  librarySelectedFacets,
} from '@/features/admin/libraries/model/librariesGrid'
import { deriveLibrarySearch } from '@/features/admin/libraries/model/librariesSearch'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

const SEARCH_DEBOUNCE_MS = 300
const PAGE_SIZE_OPTIONS = [25, 50, 100] as const

const INITIAL_GRID_STATE: GridState = {
  search: '',
  sort: { columnId: 'library', direction: 'asc' },
  filters: {},
  page: 1,
  pageSize: 50,
}

function sortParams(sort: GridState['sort']): {
  sortBy: AdminLibrarySortKey
  sortOrder: AdminLibrarySortOrder
} {
  if (!sort) return { sortBy: 'library', sortOrder: 'asc' }
  return {
    sortBy: sort.columnId as AdminLibrarySortKey,
    sortOrder: sort.direction,
  }
}

export default function LibrariesAdmin() {
  const { t, i18n } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const navigate = useNavigate()
  const { locale, setActiveLibrary, refreshSession } = useApp()
  const [gridState, setGridState] = useState<GridState>(INITIAL_GRID_STATE)
  const [selected, setSelected] = useState<AdminLibraryRow | null>(null)
  const [backupTarget, setBackupTarget] = useState<AdminLibraryRow | null>(null)
  const [restoreTarget, setRestoreTarget] = useState<AdminLibraryRow | null>(null)

  const debouncedSearch = useDebouncedValue(gridState.search, SEARCH_DEBOUNCE_MS)
  const filters = useMemo(() => librariesColumnFilters(gridState.filters), [gridState.filters])
  // One derivation owns the literal text search, its Intl-normalized spelling and
  // the typed hints over the non-text columns, so the list and every facet
  // request agree on the whole-table search without a keyword dictionary.
  const derivation = useMemo(
    () => deriveLibrarySearch(debouncedSearch, locale, t),
    [debouncedSearch, locale, t],
  )
  const { sortBy, sortOrder } = sortParams(gridState.sort)
  const offset = (gridState.page - 1) * gridState.pageSize

  const librariesQuery = useAdminLibrariesQuery({
    search: derivation.search,
    normalizedSearch: derivation.normalizedSearch,
    hints: derivation.hints,
    filters,
    sortBy,
    sortOrder,
    offset,
    limit: gridState.pageSize,
  })
  const mutations = useLibraryAdminMutations()
  const data = librariesQuery.data
  const unknownCostLabel = t('common.costUnknown')
  const unpricedCallsLabel = t('documents.unpriced')
  const formatNumber = (value: number) => new Intl.NumberFormat(i18n.language).format(value)

  const facets = useLibraryFacets({
    enabled: Boolean(data),
    locale,
    scope: {
      search: derivation.search,
      normalizedSearch: derivation.normalizedSearch,
      hints: derivation.hints,
      filters,
    },
    t,
  })

  const columns = useMemo(
    () => libraryGridColumns({ t, locale, onOpen: (row) => setSelected(row) }),
    [locale, t],
  )
  const selectedFacets = useMemo(
    () => librarySelectedFacets(gridState.filters, t, locale),
    [gridState.filters, locale, t],
  )

  const openDocuments = useCallback(
    async (row: AdminLibraryRow) => {
      try {
        const library = await mutations.loadLibrary(row.id)
        // One writer for one transition. The library carries its workspace, so
        // setting it moves the shell there; a separate workspace update from the
        // same handler only raced this one — it cleared the library, wrote a half
        // scope, and asked the server for a session this path had no need of.
        setActiveLibrary(mapCatalogLibrary(library, locale))
        await navigate('/documents')
      } catch (error: unknown) {
        toast.error(errorMessage(error, t('admin.libraries.openDocumentsFailed')))
      }
    },
    [locale, mutations, navigate, setActiveLibrary, t],
  )
  const configureAi = useCallback(
    (row: AdminLibraryRow) => {
      void navigate(`/admin/ai?scope=library&lib=${encodeURIComponent(row.id)}`)
    },
    [navigate],
  )

  const saveSettings = useCallback(
    async (
      libraryId: string,
      draft: LibrarySettingsDraft,
      current: Parameters<typeof mutations.saveSettings.mutateAsync>[0]['current'],
    ) => {
      await mutations.saveSettings.mutateAsync({ libraryId, draft, current })
      await refreshSession()
    },
    [mutations, refreshSession],
  )

  const deleteLibrary = useCallback(
    async (row: AdminLibraryRow) => {
      try {
        await mutations.deleteLibrary.mutateAsync({
          libraryId: row.id,
          workspaceId: row.workspace.id,
        })
        toast.success(t('admin.libraries.deleteCompleted', { count: 1 }))
        setSelected(null)
        await refreshSession()
      } catch (error: unknown) {
        toast.error(errorMessage(error, t('admin.libraries.deleteFailed')))
      }
    },
    [mutations.deleteLibrary, refreshSession, t],
  )

  const askDelete = useCallback(
    async (row: AdminLibraryRow) => {
      if (!window.confirm(t('admin.libraries.settings.deleteConfirm', { name: row.displayName }))) {
        return
      }
      await deleteLibrary(row)
    },
    [deleteLibrary, t],
  )

  const serviceColumns = useMemo(
    () => [
      libraryActionsColumn({
        t,
        onOpen: openDocuments,
        onConfigureAi: configureAi,
        onBackup: setBackupTarget,
        onRestore: setRestoreTarget,
        onDelete: askDelete,
        onDetails: setSelected,
        deleting: mutations.deleteLibrary.isPending,
      }),
    ],
    [askDelete, configureAi, mutations.deleteLibrary.isPending, openDocuments, t],
  )

  const totalCount = data?.totalCount ?? 0
  const hasRows = (data?.items.length ?? 0) > 0
  const canNext = hasRows && offset + (data?.items.length ?? 0) < totalCount
  const loadFailed = librariesQuery.isError && !data

  return (
    <div>
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.librariesTitle')}
        description={t('admin.librariesSubtitle')}
      />

      {data && (
        <div className="metric-grid admin-metrics">
          <div className="metric-card">
            <span>{t('admin.libraries.workspaces')}</span>
            <strong>{formatNumber(data.facets.workspaceCount)}</strong>
            <small>{t('admin.libraries.settings.filteredScope')}</small>
          </div>
          <div className="metric-card">
            <span>{t('admin.librariesTitle')}</span>
            <strong>{formatNumber(data.totalCount)}</strong>
            <small>
              {t('admin.libraries.settings.readinessSummary', {
                ready: formatNumber(data.facets.readyCount),
                blocked: formatNumber(Math.max(0, data.totalCount - data.facets.readyCount)),
              })}
            </small>
          </div>
          <div className="metric-card">
            <span>{t('admin.libraries.totalCost')}</span>
            <strong>
              {formatCost(
                data.aggregateCostSummary
                  ? { ...data.aggregateCostSummary, costRollupFreshness: 'current' }
                  : null,
                unknownCostLabel,
                unpricedCallsLabel,
                i18n.language,
              )}
            </strong>
            <small>
              {data.aggregateCostSummary
                ? t('admin.libraries.settings.providerCallsSummary', {
                    count: formatNumber(data.aggregateCostSummary.providerCallCount),
                  })
                : t('common.notAvailable')}
            </small>
          </div>
        </div>
      )}

      <Panel>
        <DataGrid<AdminLibraryRow>
          mode="remote"
          rows={data?.items ?? []}
          rowId={(row) => row.id}
          columns={columns}
          serviceColumns={serviceColumns}
          state={gridState}
          onStateChange={setGridState}
          ariaLabel={t('admin.librariesTitle')}
          searchPlaceholder={t('admin.libraries.searchPlaceholder')}
          emptyText={t('admin.libraries.empty')}
          noMatchText={t('admin.libraries.noMatches')}
          pending={librariesQuery.isLoading || librariesQuery.isFetching}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          total={data?.totalCount ?? null}
          facets={selectedFacets}
          serverFacets={facets.serverFacets}
          pagination={{
            canPrevious: gridState.page > 1,
            canNext,
            onPrevious: () =>
              setGridState((state) => ({ ...state, page: Math.max(1, state.page - 1) })),
            onNext: () => setGridState((state) => ({ ...state, page: state.page + 1 })),
            shown: hasRows ? { from: offset + 1, to: offset + (data?.items.length ?? 0) } : null,
          }}
          {...(loadFailed
            ? {
                errorText: t('admin.libraries.loadFailed'),
                onRetry: () => void librariesQuery.refetch(),
              }
            : {})}
        />
      </Panel>

      {selected && (
        <LibraryInspector
          row={selected}
          open
          saving={mutations.saveSettings.isPending}
          deleting={mutations.deleteLibrary.isPending}
          onOpenChange={(open) => !open && setSelected(null)}
          onOpenDocuments={openDocuments}
          onConfigureAi={configureAi}
          onBackup={(row) => {
            setSelected(null)
            setBackupTarget(row)
          }}
          onRestore={(row) => {
            setSelected(null)
            setRestoreTarget(row)
          }}
          onDelete={deleteLibrary}
          onSave={saveSettings}
        />
      )}
      {backupTarget && (
        <SnapshotExportDialog
          row={backupTarget}
          open
          onOpenChange={(open) => !open && setBackupTarget(null)}
        />
      )}
      {restoreTarget && (
        <SnapshotRestoreDialog
          row={restoreTarget}
          open
          busy={mutations.restoreSnapshot.isPending}
          onOpenChange={(open) => !open && setRestoreTarget(null)}
          onRestore={async (input) => {
            await mutations.restoreSnapshot.mutateAsync(input)
            await refreshSession()
          }}
        />
      )}
    </div>
  )
}
