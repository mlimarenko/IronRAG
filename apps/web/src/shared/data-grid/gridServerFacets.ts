import type { GridFacetPage, GridServerFacets } from './types'

/** The query state a facet list needs to page through its values. */
export interface GridFacetNavigation {
  /** The current page accessor for the open column's values. */
  page: (columnId: string) => GridFacetPage | undefined
  /** Records which column's menu is open. */
  setActiveColumn: (columnId: string) => void
  /** Clears the menu's own value search for a freshly opened menu. */
  clearSearch: () => void
  /** Records the term typed inside the open menu. */
  setSearch: (value: string) => void
  hasNextPage: boolean
  isFetching: boolean
  fetchNextPage: () => void
}

/**
 * The one assembly of the grid's server-facet contract from an open column and
 * the query that fetches its values. Every feature's facet hook shares it, so
 * the open/search/load-more semantics cannot drift between tables.
 */
export function buildGridServerFacets(navigation: GridFacetNavigation): GridServerFacets {
  return {
    page: navigation.page,
    onOpen: (columnId) => {
      navigation.setActiveColumn(columnId)
      navigation.clearSearch()
    },
    onSearch: (_columnId, value) => navigation.setSearch(value),
    onLoadMore: () => {
      if (navigation.hasNextPage && !navigation.isFetching) navigation.fetchNextPage()
    },
  }
}
