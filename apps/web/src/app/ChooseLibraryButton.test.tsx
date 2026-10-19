import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ChooseLibraryButton } from './ChooseLibraryButton'
import { ScopeSwitcherContext } from './scope-switcher-context'

type AppMock = { libraries: Array<{ id: string; name: string }> }

const mocks = vi.hoisted<{ app: AppMock; role: string }>(() => ({
  app: { libraries: [{ id: 'library-1', name: 'Library' }] },
  role: 'admin',
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => mocks.app,
}))

// The create branch has to admit exactly who `AdminLayout` admits, so it reads
// the same preferences role that gate reads.
vi.mock('./preferences-context', () => ({
  usePreferences: () => ({ role: mocks.role }),
}))

function renderButton({ withShell = true }: { withShell?: boolean } = {}) {
  const openScopeSwitcher = vi.fn()
  const button = <ChooseLibraryButton />
  render(
    <MemoryRouter initialEntries={['/dashboard']}>
      <Routes>
        <Route
          path="/dashboard"
          element={
            withShell ? (
              <ScopeSwitcherContext.Provider value={{ openScopeSwitcher }}>
                {button}
              </ScopeSwitcherContext.Provider>
            ) : (
              button
            )
          }
        />
        <Route path="/admin/libraries" element={<div>libraries administration</div>} />
      </Routes>
    </MemoryRouter>,
  )
  return { openScopeSwitcher }
}

describe('ChooseLibraryButton', () => {
  beforeEach(() => {
    mocks.app.libraries = [{ id: 'library-1', name: 'Library' }]
    mocks.role = 'admin'
  })

  it('opens the shell picker when there is something to pick', () => {
    const { openScopeSwitcher } = renderButton()

    fireEvent.click(screen.getByRole('button', { name: 'Choose a library' }))
    expect(openScopeSwitcher).toHaveBeenCalledTimes(1)
  })

  it('sends an administrator to create one when there are no libraries at all', () => {
    // The picker opens on an empty list reading "no matches", which suggests a
    // filter is hiding something rather than that nothing exists yet.
    mocks.app.libraries = []

    renderButton()

    expect(screen.queryByRole('button', { name: 'Choose a library' })).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Create a library' }))
    expect(screen.getByText('libraries administration')).toBeInTheDocument()
  })

  it('offers nothing to press to a reader who can neither pick nor create', () => {
    mocks.app.libraries = []
    mocks.role = 'viewer'

    renderButton()

    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('offers no picker it cannot open outside the shell', () => {
    renderButton({ withShell: false })

    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })
})
