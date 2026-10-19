import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { MemoryRouter } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { AppShell } from './AppShell'

const layoutStyles = readFileSync(resolve(process.cwd(), 'src/styles/layout.css'), 'utf8')

const mocks = vi.hoisted(() => ({
  app: {
    user: { id: 'principal-1', login: 'admin', displayName: 'Admin', role: 'admin' },
    workspaces: [{ id: 'ws-1', slug: 'default', name: 'Default workspace' }],
    libraries: [{ id: 'library-1', name: 'Library' }],
    activeWorkspace: { id: 'ws-1', slug: 'default', name: 'Default workspace' },
    activeLibrary: { id: 'library-1', name: 'Library' },
    setActiveWorkspace: vi.fn(),
    setActiveLibrary: vi.fn(),
    logout: vi.fn(),
    sessionError: null as string | null,
  },
  prefs: {
    role: 'admin',
    theme: 'light',
    locale: 'en',
    devMode: false,
    sidebarCollapsed: false,
    setTheme: vi.fn(),
    setLocale: vi.fn(),
    setDevMode: vi.fn(),
    setSidebarCollapsed: vi.fn(),
  },
  getLatestVersion: vi.fn(),
}))

vi.mock('@/shared/contexts/app-context', () => ({ useApp: () => mocks.app }))
vi.mock('./preferences-context', () => ({ usePreferences: () => mocks.prefs }))
vi.mock('@/shared/api/version', () => ({
  versionApi: { getLatestVersion: mocks.getLatestVersion },
}))

function renderShell() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/dashboard']}>
        <AppShell>
          <div>page body</div>
        </AppShell>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

function overlay(): HTMLElement | null {
  return document.querySelector('.mobile-overlay')
}

function drawerIsOpen(): boolean {
  return document.querySelector('.mobile-drawer')?.classList.contains('is-open') ?? false
}

describe('AppShell mobile drawer backdrop', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.getLatestVersion.mockResolvedValue({ version: '0.0.0', releasedAt: null })
  })

  it('dismisses the drawer from the keyboard alone', async () => {
    const user = userEvent.setup()
    renderShell()

    const trigger = screen.getByRole('button', { name: 'Menu' })
    await user.click(trigger)
    expect(drawerIsOpen()).toBe(true)
    expect(screen.getByRole('dialog')).toContainElement(document.activeElement as HTMLElement)

    await user.keyboard('{Escape}')
    expect(drawerIsOpen()).toBe(false)
    expect(overlay()).toBeNull()
    expect(trigger).toHaveFocus()
  })

  it('keeps the backdrop a named button and dismisses on pointer activation', async () => {
    const user = userEvent.setup()
    renderShell()

    await user.click(screen.getByRole('button', { name: 'Menu' }))

    expect(overlay()).toHaveAttribute('aria-label', 'Close')
    expect(overlay()?.tagName).toBe('BUTTON')
    await user.click(overlay() as HTMLElement)
    expect(drawerIsOpen()).toBe(false)
  })
})

describe('AppShell sidebar label clipping', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.getLatestVersion.mockResolvedValue({ version: '0.0.0', releasedAt: null })
  })

  /**
   * jsdom only resolves the cascade for stylesheets it actually parses, so the
   * contract has to be injected the way the app loads it. A long label used to
   * be cut mid-glyph at the sidebar edge with no ellipsis because the flex
   * child defaulted to `min-width: auto`; the assertions below fail on that
   * CSS and pass once the span may shrink and the text is ellipsized.
   */
  it('ellipsizes a clipped nav label instead of cutting it mid-glyph', () => {
    const style = document.createElement('style')
    style.textContent = layoutStyles
    document.head.append(style)

    try {
      renderShell()

      const linkLabel = document.querySelector<HTMLElement>('.primary-nav a span')
      const groupHeading = document.querySelector<HTMLElement>('.nav-label')
      expect(linkLabel).not.toBeNull()
      expect(groupHeading).not.toBeNull()

      const labelStyle = getComputedStyle(linkLabel as HTMLElement)
      expect(labelStyle.textOverflow).toBe('ellipsis')
      expect(labelStyle.overflow).toBe('hidden')
      expect(labelStyle.minWidth).toBe('0px')

      const headingStyle = getComputedStyle(groupHeading as HTMLElement)
      expect(headingStyle.textOverflow).toBe('ellipsis')
      expect(headingStyle.overflow).toBe('hidden')
    } finally {
      style.remove()
    }
  })
})

describe('AppShell compact hierarchy', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.app.user.role = 'admin'
    mocks.app.sessionError = null
    mocks.prefs.sidebarCollapsed = false
    mocks.getLatestVersion.mockResolvedValue({ status: 'current' })
  })

  it('keeps every route, groups admin links, and provides a focusable skip target', () => {
    const { container } = renderShell()
    const nav = screen.getByRole('navigation', { name: 'Navigation' })
    expect(
      within(nav)
        .getAllByRole('link')
        .map((link) => link.getAttribute('href')),
    ).toEqual([
      '/dashboard',
      '/documents',
      '/graph',
      '/assistant',
      '/admin/libraries',
      '/admin/users',
      '/admin/access',
      '/admin/ai',
      '/admin/webhooks',
      '/admin/audit',
      '/admin/system',
    ])
    expect(container.querySelectorAll('.nav-label')).toHaveLength(3)
    expect(container.querySelector('.skip-link')).toHaveAttribute('href', '#main-content')
    expect(screen.getByRole('main')).toHaveAttribute('id', 'main-content')
    expect(screen.getByRole('main')).toHaveAttribute('tabindex', '-1')
    expect(container.querySelector('.mobile-drawer')).toBeNull()
  })

  it('filters admin routes for a non-admin', () => {
    mocks.app.user.role = 'reader'
    renderShell()
    expect(
      screen.getAllByRole('link').some((link) => link.getAttribute('href')?.startsWith('/admin')),
    ).toBe(false)
  })

  it('names the collapsed-sidebar control and keeps mobile navigation expanded', async () => {
    mocks.prefs.sidebarCollapsed = true
    const user = userEvent.setup()
    renderShell()
    await user.click(screen.getByRole('button', { name: 'Expand navigation' }))
    expect(mocks.prefs.setSidebarCollapsed).toHaveBeenCalledWith(false)
    await user.click(screen.getByRole('button', { name: 'Menu' }))
    const drawer = screen.getByRole('dialog', { name: 'Menu' })
    expect(within(drawer).getByRole('link', { name: 'Documents' })).toHaveTextContent('Documents')
    await user.click(within(drawer).getByRole('link', { name: 'Documents' }))
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('keeps theme, language, user details and logout in the compact header', async () => {
    const user = userEvent.setup()
    renderShell()
    await user.click(screen.getByRole('button', { name: 'Theme' }))
    expect(mocks.prefs.setTheme).toHaveBeenCalledWith('dark')
    await user.click(screen.getByRole('button', { name: 'Language' }))
    expect(mocks.prefs.setLocale).toHaveBeenCalledWith('ru')
    await user.click(screen.getByRole('button', { name: 'User menu' }))
    expect(screen.getByRole('menu')).toHaveTextContent('Adminadmin')
    await user.click(screen.getByRole('menuitem', { name: 'Sign out' }))
    expect(mocks.app.logout).toHaveBeenCalledTimes(1)
  })

  it('opens the command palette by keyboard and the scope switcher by header control', async () => {
    const user = userEvent.setup()
    renderShell()
    await user.keyboard('{Control>}k{/Control}')
    expect(screen.getByRole('dialog', { name: 'Search' })).toBeInTheDocument()
    await user.keyboard('{Escape}')
    await user.click(screen.getByRole('button', { name: /Default workspace.*Library/ }))
    expect(screen.getByRole('dialog', { name: 'Switch workspace & library' })).toBeInTheDocument()
  })

  it('preserves session and release notices', async () => {
    mocks.app.sessionError = 'Session temporarily unavailable'
    mocks.getLatestVersion.mockResolvedValue({
      status: 'update_available',
      latestVersion: '9.0.0',
      releaseUrl: 'https://example.com/release',
    })
    renderShell()
    expect(screen.getByRole('status')).toHaveTextContent('Session temporarily unavailable')
    expect(screen.getByRole('status').tagName).toBe('OUTPUT')
    const update = await screen.findByRole('link', { name: /Update available/ })
    expect(update).toHaveAttribute('href', 'https://example.com/release')
    expect(screen.getByRole('link', { name: 'API docs' })).toHaveAttribute('href', '/swagger.html')
  })
})
