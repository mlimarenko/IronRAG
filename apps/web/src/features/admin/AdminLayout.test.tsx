import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { describe, expect, it, vi } from 'vitest'

import AdminLayout from './AdminLayout'

const mocks = vi.hoisted<{ role: 'admin' | 'member' }>(() => ({ role: 'member' }))

vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => ({ role: mocks.role }),
}))

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}))

// The eight administration screens pull in the whole admin surface (charts,
// editors, generated API clients). This suite is about what a reader without
// admin rights sees, so the screens themselves are stubbed out.
vi.mock('./AccessAdmin', () => ({ default: () => <div /> }))
vi.mock('./AiAdmin', () => ({ default: () => <div /> }))
vi.mock('./AuditAdmin', () => ({ default: () => <div /> }))
vi.mock('./LibrariesAdmin', () => ({ default: () => <div>libraries screen</div> }))
vi.mock('./QueueAdmin', () => ({ default: () => <div /> }))
vi.mock('./SystemAdmin', () => ({ default: () => <div /> }))
vi.mock('./UsersAdmin', () => ({ default: () => <div /> }))
vi.mock('./WebhooksAdmin', () => ({ default: () => <div /> }))

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/admin/*" element={<AdminLayout />} />
        <Route path="/dashboard" element={<div>dashboard screen</div>} />
      </Routes>
    </MemoryRouter>,
  )
}

describe('AdminLayout access boundary', () => {
  it('explains the closed section instead of silently bouncing the reader away', () => {
    mocks.role = 'member'

    renderAt('/admin/users')

    // A bare redirect made a shared link or a stale bookmark look broken: the
    // reader landed back on the dashboard with nothing said about why.
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('admin.noAccessTitle')
    expect(screen.getByText('admin.noAccessDetail')).toBeInTheDocument()
    expect(screen.getByText('admin.noAccessHint')).toBeInTheDocument()
    expect(screen.queryByText('dashboard screen')).not.toBeInTheDocument()
  })

  it('offers a way out named after the screen it leads to', () => {
    mocks.role = 'member'

    renderAt('/admin/system')

    // The button carries the sidebar label for its destination, so the exit is
    // recognisable as the same place the navigation calls it.
    expect(screen.getByRole('button', { name: 'nav.dashboard' })).toBeInTheDocument()
  })

  it('still routes an administrator to the requested screen', () => {
    mocks.role = 'admin'

    renderAt('/admin/libraries')

    expect(screen.getByText('libraries screen')).toBeInTheDocument()
    expect(screen.queryByText('admin.noAccessTitle')).not.toBeInTheDocument()
  })

  it('tells an administrator a stale admin link is stale, without growing the address', () => {
    // The catch-all used to redirect to a relative "libraries". Under a splat
    // route that appends rather than replaces, so /admin/tokens became
    // /admin/tokens/libraries, matched the catch-all again, and climbed until
    // the address was kilobytes long and nothing had rendered.
    mocks.role = 'admin'

    const { container } = renderAt('/admin/tokens')

    expect(screen.getByText('notFound.title')).toBeInTheDocument()
    expect(container.textContent).not.toContain('libraries screen')
  })
})
