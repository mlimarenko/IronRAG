import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { auditApi, type AuditEventPage } from '@/shared/api'
import type { Locale } from '@/shared/types'

import { useAuditEvents, useAuditFacets, useAuditSelectedValues, type AuditScope } from './api'

// A fake translator mirroring the owning i18n contract: the closed audit
// vocabularies are the only keys the derivation reads.
const vocabularies: Record<string, Record<string, string>> = {
  auditActionLabels: { 'iam.session.login': 'Вход в систему' },
  auditResultLabels: { failed: 'Ошибка', rejected: 'Отклонено', succeeded: 'Успех' },
  auditSurfaceLabels: { bootstrap: 'Bootstrap', mcp: 'MCP', rest: 'REST', worker: 'Worker' },
}

const t = ((key: string, options?: { returnObjects?: boolean; defaultValue?: unknown }) => {
  const namespace = /^admin\.(\w+)$/.exec(key)?.[1]
  if (namespace) {
    if (options?.returnObjects) return vocabularies[namespace] ?? {}
    return key
  }
  const label = /^admin\.(\w+)\.(.+)$/.exec(key)
  if (label && vocabularies[label[1]!]?.[label[2]!]) return vocabularies[label[1]!]![label[2]!]
  if (key === 'dataGrid.nullValue') return '∅'
  return options?.defaultValue ?? key
}) as never

const locale: Locale = 'ru'

const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
const wrapper = ({ children }: Readonly<{ children: ReactNode }>) =>
  createElement(QueryClientProvider, { client: queryClient }, children)

function scope(overrides: Partial<AuditScope> = {}): AuditScope {
  return {
    workspaceId: '',
    libraryId: '',
    internal: false,
    includeAssistant: false,
    filters: undefined,
    search: '',
    sort: null,
    limit: 50,
    ...overrides,
  }
}

function page(): AuditEventPage {
  return { items: [], nextCursor: null, total: 0 }
}

function facets() {
  return { hasMore: false, values: [] }
}

afterEach(() => {
  queryClient.clear()
  vi.restoreAllMocks()
})

describe('audit global search hints', () => {
  it('keeps the old cursor only as placeholder data while a different sort loads', async () => {
    const oldPage = { ...page(), nextCursor: 'created-at-cursor' }
    let finish!: (value: ReturnType<typeof page>) => void
    const replacement = new Promise<ReturnType<typeof page>>((resolve) => {
      finish = resolve
    })
    vi.spyOn(auditApi, 'list').mockResolvedValueOnce(oldPage).mockReturnValueOnce(replacement)
    const initialProps: Pick<AuditScope, 'sort'> = { sort: null }
    const { result, rerender } = renderHook(
      ({ sort }: Pick<AuditScope, 'sort'>) => useAuditEvents(scope({ sort }), null, t, locale),
      { wrapper, initialProps },
    )
    await waitFor(() => expect(result.current.data?.nextCursor).toBe('created-at-cursor'))
    rerender({ sort: { columnId: 'action', direction: 'asc' } })
    await waitFor(() => expect(result.current.isPlaceholderData).toBe(true))
    expect(result.current.isLoading).toBe(false)
    await act(async () => {
      finish({ ...page(), nextCursor: 'action-cursor' })
    })
    await waitFor(() => expect(result.current.isPlaceholderData).toBe(false))
    expect(result.current.data?.nextCursor).toBe('action-cursor')
  })
  it('derives one searchHints JSON per hint kind and passes the raw term too', async () => {
    const list = vi.spyOn(auditApi, 'list').mockResolvedValue(page())

    const cases: ReadonlyArray<{ search: string; expected: Record<string, unknown> }> = [
      { search: 'Ошибка', expected: { resultKinds: ['failed'] } },
      { search: 'Вход в систему', expected: { actionKinds: ['iam.session.login'] } },
      { search: 'REST', expected: { surfaceKinds: ['rest'] } },
      { search: '∅', expected: { actorNull: true } },
    ]
    for (const { search, expected } of cases) {
      list.mockClear()
      const { unmount } = renderHook(() => useAuditEvents(scope({ search }), null, t, locale), {
        wrapper,
      })
      await waitFor(() => expect(list).toHaveBeenCalledTimes(1))
      const params = list.mock.calls[0]![0]
      expect(params).toMatchObject({ search })
      expect(params.searchHints).toMatchObject(expected)
      unmount()
    }

    list.mockClear()
    const { unmount } = renderHook(
      () => useAuditEvents(scope({ search: '02.08.2026, 10:00:00' }), null, t, locale),
      { wrapper },
    )
    await waitFor(() => expect(list).toHaveBeenCalledTimes(1))
    expect(list.mock.calls[0]![0].searchHints).toMatchObject({
      createdAtRange: expect.objectContaining({ min: expect.any(String), max: expect.any(String) }),
    })
    unmount()
  })
})

describe('audit facet hints', () => {
  it('preserves the global hints while sending the opened column hints separately', async () => {
    const facetsSpy = vi.spyOn(auditApi, 'facets').mockResolvedValue(facets())
    const { result } = renderHook(
      () => useAuditFacets(scope({ search: 'Ошибка' }), true, locale, t),
      { wrapper },
    )

    await act(async () => {
      result.current.serverFacets.onOpen('created_at')
    })
    await act(async () => {
      result.current.serverFacets.onSearch('created_at', '02.08.2026, 10:00:00')
    })

    await waitFor(() => expect(facetsSpy).toHaveBeenCalled())
    await waitFor(() => expect(result.current.serverFacets.page('created_at')?.pending).toBe(false))
    const params = facetsSpy.mock.calls.at(-1)![0]
    // The whole-table term keeps its own hint set...
    expect(params.searchHints).toEqual({ resultKinds: ['failed'] })
    // ...and the opened date column resolves only its own interval.
    expect(params.facetSearchHints).toMatchObject({
      createdAtRange: expect.objectContaining({ min: expect.any(String), max: expect.any(String) }),
    })
    expect(params.facetSearchHints).not.toHaveProperty('resultKinds')
  })

  it('never forwards another column vocabulary from a text/actor column search', async () => {
    const facetsSpy = vi.spyOn(auditApi, 'facets').mockResolvedValue(facets())
    const { result } = renderHook(() => useAuditFacets(scope({ search: '' }), true, locale, t), {
      wrapper,
    })

    await act(async () => {
      result.current.serverFacets.onOpen('actor')
    })
    await act(async () => {
      result.current.serverFacets.onSearch('actor', 'Ошибка')
    })

    await waitFor(() => expect(facetsSpy).toHaveBeenCalled())
    await waitFor(() => expect(result.current.serverFacets.page('actor')?.pending).toBe(false))
    const params = facetsSpy.mock.calls.at(-1)![0]
    expect(params.facetColumn).toBe('actor')
    expect(params.facetSearch).toBe('Ошибка')
    // A result-label term resolved on the actor column adds nothing: the actor
    // formatter has no enum vocabulary to widen the request with.
    expect(params.facetSearchHints).toBeUndefined()
    expect(params.searchHints).toBeUndefined()
  })
})

describe('audit selected-value labels', () => {
  it('looks up only the selected actor ids and keeps the same global hints', async () => {
    const facetsSpy = vi.spyOn(auditApi, 'facets').mockResolvedValue({
      hasMore: false,
      values: [{ value: 'actor-1', label: 'Ada Lovelace', count: 1 }],
    })
    const { result } = renderHook(
      () =>
        useAuditSelectedValues(
          scope({ search: 'Ошибка' }),
          { actor: ['string:actor-1'] },
          locale,
          t,
        ),
      { wrapper },
    )

    await waitFor(() => expect(facetsSpy).toHaveBeenCalledTimes(1))
    const params = facetsSpy.mock.calls[0]![0]
    expect(params.searchHints).toEqual({ resultKinds: ['failed'] })
    expect(params.selectedValues).toEqual([{ column: 'actor', value: 'actor-1', label: '' }])
    expect(params).toMatchObject({ facetColumn: 'actor', facetLimit: 1 })
    await waitFor(() =>
      expect(result.current.resolveLabel('actor', 'string:actor-1')).toBe('Ada Lovelace'),
    )
  })

  it('sends only the actor selected values for a mixed actor/enum/date selection', async () => {
    const facetsSpy = vi.spyOn(auditApi, 'facets').mockResolvedValue(facets())
    renderHook(
      () =>
        useAuditSelectedValues(
          scope(),
          {
            actor: ['string:actor-1'],
            result: ['string:failed'],
            created_at: ['string:2026-08-09 10:00:00+00'],
          },
          locale,
          t,
        ),
      { wrapper },
    )

    await waitFor(() => expect(facetsSpy).toHaveBeenCalledTimes(1))
    const params = facetsSpy.mock.calls[0]![0]
    // The enum and date labels are canonical locally, so they never travel to
    // the one `facetColumn` projection the endpoint takes.
    expect(params.selectedValues).toEqual([{ column: 'actor', value: 'actor-1', label: '' }])
    expect(params).toMatchObject({ facetColumn: 'actor', facetLimit: 1 })
  })

  it('makes no selected-label request for enum/date-only filters', async () => {
    const facetsSpy = vi.spyOn(auditApi, 'facets').mockResolvedValue(facets())
    const { result } = renderHook(
      () =>
        useAuditSelectedValues(
          scope(),
          {
            result: ['string:failed'],
            created_at: ['string:2026-08-09 10:00:00+00'],
          },
          locale,
          t,
        ),
      { wrapper },
    )

    // No actor id means nothing the server owns; the grid renders the enum and
    // date labels through its own formatters. `enabled` is false, so the query
    // never schedules a request at all.
    expect(result.current.selectedValues).toEqual([])
    expect(facetsSpy).not.toHaveBeenCalled()
  })
})
