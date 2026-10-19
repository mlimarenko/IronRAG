import { useEffect, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Building2, Check, Library as LibraryIcon, LoaderCircle, Search } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Catalog } from '@/shared/api'
import { mapCatalogLibrary } from '@/shared/lib/library'
import { unwrap } from '@/shared/api/runtime'
import { useApp } from '@/shared/contexts/app-context'
import { Modal } from '@/shared/ui'

const LIBRARY_PAGE_LIMIT = 20

// Workspace + library scope picker. Workspaces come from the resolved shell
// bootstrap (already in AppContext); libraries are searched server-side because
// the shell only carries a bounded sample.
export function ScopeSwitcher({
  open,
  onOpenChange,
}: Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
}>) {
  const { t } = useTranslation()
  const {
    workspaces,
    activeWorkspace,
    activeLibrary,
    locale,
    setActiveWorkspace,
    setActiveLibrary,
  } = useApp()
  const [workspaceFilter, setWorkspaceFilter] = useState('')
  const [librarySearch, setLibrarySearch] = useState('')
  const [debounced, setDebounced] = useState('')
  const [libraryLimit, setLibraryLimit] = useState(LIBRARY_PAGE_LIMIT)

  const handleOpenChange = (next: boolean) => {
    if (!next) {
      setWorkspaceFilter('')
      setLibrarySearch('')
      setDebounced('')
      setLibraryLimit(LIBRARY_PAGE_LIMIT)
    }
    onOpenChange(next)
  }

  useEffect(() => {
    const id = window.setTimeout(() => setDebounced(librarySearch), 250)
    return () => window.clearTimeout(id)
  }, [librarySearch])

  const visibleWorkspaces = useMemo(
    () => workspaces.filter((ws) => ws.name.toLowerCase().includes(workspaceFilter.toLowerCase())),
    [workspaces, workspaceFilter],
  )

  const libraryQuery = useQuery({
    queryKey: ['scope', 'libraries', activeWorkspace?.id, debounced, libraryLimit],
    enabled: open && !!activeWorkspace,
    queryFn: async () => {
      const page = unwrap(
        await Catalog.listCatalogLibraries({
          path: { workspaceId: activeWorkspace!.id },
          query: debounced ? { search: debounced, limit: libraryLimit } : { limit: libraryLimit },
        }),
      )
      return { items: page.items, total: page.totalCount }
    },
  })

  return (
    <Modal open={open} onOpenChange={handleOpenChange} title={t('shell.switchScope')} wide>
      <div className="scope-switcher">
        <section>
          <header>
            <Building2 size={15} />
            <span>{t('common.workspace')}</span>
          </header>
          <div className="scope-search">
            <Search size={15} />
            <input
              value={workspaceFilter}
              onChange={(e) => setWorkspaceFilter(e.target.value)}
              placeholder={t('shell.searchWorkspaces')}
            />
          </div>
          <ul className="scope-list" role="listbox" aria-label={t('common.workspace')}>
            {visibleWorkspaces.map((ws) => (
              <li key={ws.id}>
                <button
                  type="button"
                  role="option"
                  aria-selected={ws.id === activeWorkspace?.id}
                  className={ws.id === activeWorkspace?.id ? 'is-active' : ''}
                  onClick={() => {
                    if (ws.id === activeWorkspace?.id) return
                    setLibrarySearch('')
                    setDebounced('')
                    setLibraryLimit(LIBRARY_PAGE_LIMIT)
                    // One action: the context clears the library that belongs
                    // to the workspace being left. Clearing it here as a second
                    // call is what wrote the previous workspace back over the
                    // one just chosen.
                    setActiveWorkspace(ws)
                  }}
                >
                  <span>{ws.name}</span>
                  {ws.id === activeWorkspace?.id && <Check size={15} />}
                </button>
              </li>
            ))}
            {visibleWorkspaces.length === 0 && (
              <li className="scope-empty">{t('common.noMatches')}</li>
            )}
          </ul>
        </section>
        <section>
          <header>
            <LibraryIcon size={15} />
            <span>{t('common.library')}</span>
          </header>
          <div className="scope-search">
            <Search size={15} />
            <input
              value={librarySearch}
              onChange={(e) => {
                setLibrarySearch(e.target.value)
                setLibraryLimit(LIBRARY_PAGE_LIMIT)
              }}
              placeholder={t('shell.searchLibraries')}
              disabled={!activeWorkspace}
            />
          </div>
          {activeWorkspace && (
            <div className="scope-list-summary" aria-live="polite">
              {libraryQuery.data && (
                <>
                  <span>
                    {t('shell.libraryCount', {
                      shown: libraryQuery.data.items.length,
                      total: libraryQuery.data.total,
                    })}
                  </span>
                  {libraryQuery.data.total > libraryQuery.data.items.length && (
                    <span>{t('shell.refineLibrarySearch')}</span>
                  )}
                </>
              )}
              {libraryQuery.isFetching && (
                <LoaderCircle className="spin" size={14} aria-label={t('common.loading')} />
              )}
            </div>
          )}
          <ul className="scope-list" role="listbox" aria-label={t('common.library')}>
            {libraryQuery.isLoading && <li className="scope-empty">{t('common.loading')}</li>}
            {libraryQuery.isError && <li className="scope-empty">{t('common.error')}</li>}
            {libraryQuery.data?.items.map((lib) => (
              <li key={lib.id}>
                <button
                  type="button"
                  role="option"
                  aria-selected={lib.id === activeLibrary?.id}
                  className={lib.id === activeLibrary?.id ? 'is-active' : ''}
                  onClick={() => {
                    setActiveLibrary(mapCatalogLibrary(lib, locale))
                    handleOpenChange(false)
                  }}
                >
                  <span>{lib.displayName}</span>
                  {lib.id === activeLibrary?.id && <Check size={15} />}
                </button>
              </li>
            ))}
            {libraryQuery.data?.items.length === 0 && (
              <li className="scope-empty">{t('common.noMatches')}</li>
            )}
            {libraryQuery.data && libraryQuery.data.total > libraryQuery.data.items.length && (
              <li>
                <button
                  className="scope-show-more"
                  type="button"
                  disabled={libraryQuery.isFetching}
                  onClick={() =>
                    setLibraryLimit((limit) =>
                      Math.min(limit + LIBRARY_PAGE_LIMIT, libraryQuery.data.total),
                    )
                  }
                >
                  {t('common.showMore')}
                </button>
              </li>
            )}
          </ul>
        </section>
      </div>
    </Modal>
  )
}
