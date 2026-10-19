import { renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'

import { adminGroupLabelKeyForPath, adminNav } from '@/app/navigation'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

function at(path: string) {
  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return <MemoryRouter initialEntries={[path]}>{children}</MemoryRouter>
  }
  return renderHook(() => useAdminBreadcrumb(), { wrapper: Wrapper }).result.current
}

describe('administration breadcrumb', () => {
  it('names the sidebar group the screen sits in, not just the section', () => {
    // "Administration" on all eight screens named a heading the sidebar no
    // longer shows and never said where in it the reader was.
    expect(at('/admin/access')).toBe('Administration · People and access')
    expect(at('/admin/libraries')).toBe('Administration · Documents and processing')
    expect(at('/admin/system')).toBe('Administration · System settings')
  })

  it('falls back to the section alone for a path the sidebar does not list', () => {
    expect(at('/admin/not-a-screen')).toBe('Administration')
  })

  it('resolves a group for every routed administration screen', () => {
    for (const [groupLabelKey, entries] of adminNav) {
      for (const [route] of entries) {
        expect(adminGroupLabelKeyForPath(route), route).toBe(groupLabelKey)
      }
    }
  })
})
