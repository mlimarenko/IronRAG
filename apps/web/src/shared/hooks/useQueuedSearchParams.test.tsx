import { act, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter, useNavigate } from 'react-router-dom'
import { describe, expect, it } from 'vitest'

import { useQueuedSearchParams } from './useQueuedSearchParams'

function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
  return (
    <MemoryRouter initialEntries={['/list?q=earlier', '/list?q=current']} initialIndex={1}>
      {children}
    </MemoryRouter>
  )
}

function paramsHook() {
  return renderHook(
    () => {
      const [params, update] = useQueuedSearchParams()
      return { params, update, navigate: useNavigate() }
    },
    { wrapper: Wrapper },
  )
}

describe('useQueuedSearchParams', () => {
  it('merges updates before a render without mutating the committed URL', () => {
    const { result } = paramsHook()
    const committed = result.current.params
    act(() => {
      result.current.update((params) => {
        params.set('filters', 'first')
        return params
      })
      result.current.update((params) => {
        params.set('sort', 'name:asc')
        return params
      })
    })
    expect(committed.toString()).toBe('q=current')
    expect(result.current.params.get('filters')).toBe('first')
    expect(result.current.params.get('sort')).toBe('name:asc')
  })

  it('uses the navigated URL after back and forward, without reviving pending filters', () => {
    const { result } = paramsHook()
    act(() =>
      result.current.update((params) => {
        params.set('filters', 'current')
        return params
      }),
    )
    act(() => {
      void result.current.navigate(-1)
    })
    act(() =>
      result.current.update((params) => {
        params.set('sort', 'earlier')
        return params
      }),
    )
    expect(result.current.params.get('q')).toBe('earlier')
    expect(result.current.params.has('filters')).toBe(false)
    act(() => {
      void result.current.navigate(1)
    })
    act(() =>
      result.current.update((params) => {
        params.set('pageSize', '25')
        return params
      }),
    )
    expect(result.current.params.get('q')).toBe('current')
    expect(result.current.params.get('filters')).toBe('current')
    expect(result.current.params.has('sort')).toBe(false)
  })
})
