import { Fragment, useEffect, useMemo, useState, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import * as DropdownMenu from '@radix-ui/react-dropdown-menu'
import * as Dialog from '@radix-ui/react-dialog'
import {
  AlertTriangle,
  BarChart3,
  ChevronsUpDown,
  ExternalLink,
  Command,
  Languages,
  LogOut,
  Menu,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Sun,
  UserRound,
  X,
} from 'lucide-react'
import { NavLink, useNavigate } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { versionApi } from '@/shared/api/version'
import { useApp } from '@/shared/contexts/app-context'
import { BUILD_VERSION_TAG, formatVersionTag } from '@/shared/lib/build-version'
import {
  readTelemetryConsentState,
  subscribeTelemetryConsent,
  type TelemetryConsentState,
} from '@/shared/lib/telemetry/consent'
import { requestTelemetryConsent } from '@/shared/lib/telemetry/accountConsent'
import { Badge, Button, IconButton } from '@/shared/ui'
import { Brand } from './Brand'
import { CommandPalette } from './CommandPalette'
import { adminNav, mainNav } from './navigation'
import { ScopeSwitcher } from './ScopeSwitcher'
import { ScopeSwitcherContext } from './scope-switcher-context'
import { usePreferences } from './preferences-context'

const REMEMBERED_RELEASE_MS = 12 * 60 * 60 * 1000
const REPOSITORY_URL = 'https://github.com/mlimarenko/IronRAG'
const RELEASES_URL = `${REPOSITORY_URL}/releases`

function releaseDestination(value: string | null | undefined): string {
  try {
    const url = new URL(value ?? '')
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : RELEASES_URL
  } catch {
    return RELEASES_URL
  }
}

export function AppShell({ children }: Readonly<{ children: ReactNode }>) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const prefs = usePreferences()
  const { user, activeWorkspace, activeLibrary, logout, sessionError } = useApp()
  const [mobileOpen, setMobileOpen] = useState(false)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [scopeOpen, setScopeOpen] = useState(false)
  const [telemetryConsent, setTelemetryConsent] = useState<TelemetryConsentState>(() =>
    readTelemetryConsentState(),
  )
  const scopeSwitcherControl = useMemo(
    () => ({ openScopeSwitcher: () => setScopeOpen(true) }),
    [setScopeOpen],
  )

  useEffect(
    () => subscribeTelemetryConsent(() => setTelemetryConsent(readTelemetryConsentState())),
    [],
  )

  const telemetryEnabled = telemetryConsent.consent === 'granted'
  const telemetryAvailable =
    telemetryConsent.status === 'anonymous' || telemetryConsent.status === 'ready'
  const telemetryLabel = telemetryAvailable
    ? t(telemetryEnabled ? 'analytics.menuDisable' : 'analytics.menuEnable')
    : t('analytics.menuUnavailable')

  const isAdmin = user?.role === 'admin'
  const release = useQuery({
    queryKey: ['shell', 'latest-version'],
    queryFn: () => versionApi.getLatestVersion(),
    refetchInterval: REMEMBERED_RELEASE_MS,
    staleTime: REMEMBERED_RELEASE_MS,
  })
  const latestVersion = release.data?.latestVersion?.trim()

  const initials = (user?.displayName ?? user?.login ?? '?')
    .split(/\s+/)
    .map((part) => part[0])
    .slice(0, 2)
    .join('')
    .toUpperCase()

  useEffect(() => {
    const desktop = window.matchMedia('(min-width: 768px)')
    const closeDrawer = () => {
      if (desktop.matches) setMobileOpen(false)
    }
    desktop.addEventListener('change', closeDrawer)
    return () => desktop.removeEventListener('change', closeDrawer)
  }, [])

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        setPaletteOpen(true)
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  const navigation = (compact: boolean) => (
    <>
      <div className="sidebar-brand">
        <Brand compact={compact} />
      </div>
      <nav className="primary-nav" aria-label={t('shell.navigation')}>
        {mainNav.map(([href, label, Icon]) => (
          <NavLink key={href} to={href} title={t(label)} onClick={() => setMobileOpen(false)}>
            <Icon size={16} aria-hidden="true" />
            <span>{t(label)}</span>
          </NavLink>
        ))}
        {isAdmin &&
          adminNav.map(([groupLabel, items]) => (
            <Fragment key={groupLabel}>
              <div className="nav-label">{!compact && t(groupLabel)}</div>
              {items.map(([href, label, Icon]) => (
                <NavLink key={href} to={href} title={t(label)} onClick={() => setMobileOpen(false)}>
                  <Icon size={16} aria-hidden="true" />
                  <span>{t(label)}</span>
                </NavLink>
              ))}
            </Fragment>
          ))}
      </nav>
      <div className="sidebar-footer">
        <button
          type="button"
          className="collapse-button"
          aria-label={t(prefs.sidebarCollapsed ? 'shell.expand' : 'shell.collapse')}
          title={t(prefs.sidebarCollapsed ? 'shell.expand' : 'shell.collapse')}
          onClick={() => prefs.setSidebarCollapsed(!prefs.sidebarCollapsed)}
        >
          {prefs.sidebarCollapsed ? (
            <PanelLeftOpen size={16} aria-hidden="true" />
          ) : (
            <PanelLeftClose size={16} aria-hidden="true" />
          )}
          <span>{t(prefs.sidebarCollapsed ? 'shell.expand' : 'shell.collapse')}</span>
        </button>
      </div>
    </>
  )

  return (
    <Dialog.Root open={mobileOpen} onOpenChange={setMobileOpen}>
      <div className={`app-shell ${prefs.sidebarCollapsed ? 'app-shell--collapsed' : ''}`}>
        <a className="skip-link" href="#main-content">
          {t('shell.skipToContent')}
        </a>
        <aside className="sidebar">{navigation(prefs.sidebarCollapsed)}</aside>
        <Dialog.Portal>
          <Dialog.Overlay asChild>
            <button
              type="button"
              className="mobile-overlay"
              aria-label={t('common.close')}
              onClick={() => setMobileOpen(false)}
            />
          </Dialog.Overlay>
          <Dialog.Content asChild aria-describedby={undefined}>
            <aside className="mobile-drawer is-open">
              <Dialog.Title className="sr-only">{t('shell.menu')}</Dialog.Title>
              <Dialog.Close asChild>
                <IconButton label={t('common.close')} onClick={() => setMobileOpen(false)}>
                  <X size={19} />
                </IconButton>
              </Dialog.Close>
              {navigation(false)}
            </aside>
          </Dialog.Content>
        </Dialog.Portal>
        <div className="app-main">
          <header className="topbar">
            <Dialog.Trigger asChild>
              <IconButton
                className="mobile-menu"
                label={t('shell.menu')}
                onClick={() => setMobileOpen(true)}
              >
                <Menu size={20} />
              </IconButton>
            </Dialog.Trigger>
            <button type="button" className="context-switcher" onClick={() => setScopeOpen(true)}>
              <span className="context-kicker">
                {activeWorkspace?.name ?? t('shell.noWorkspace')}
              </span>
              <span className="context-divider" aria-hidden="true">
                /
              </span>
              <strong>{activeLibrary?.name ?? t('shell.noLibrary')}</strong>
              <ChevronsUpDown size={16} />
            </button>
            <button type="button" className="command-trigger" onClick={() => setPaletteOpen(true)}>
              <Command size={16} />
              <span>{t('shell.command')}</span>
              <kbd>⌘ K</kbd>
            </button>
            <div className="topbar-actions">
              <Button
                size="icon"
                variant="ghost"
                onClick={() => prefs.setTheme(prefs.theme === 'dark' ? 'light' : 'dark')}
                aria-label={t('common.theme')}
              >
                {prefs.theme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
              </Button>
              <Button
                size="icon"
                variant="ghost"
                onClick={() => prefs.setLocale(prefs.locale === 'en' ? 'ru' : 'en')}
                aria-label={t('common.language')}
              >
                <Languages size={17} />
              </Button>
              <DropdownMenu.Root>
                <DropdownMenu.Trigger asChild>
                  <button className="user-chip" type="button" aria-label={t('shell.userMenu')}>
                    <span>{initials}</span>
                    <strong>{user?.displayName ?? user?.login}</strong>
                  </button>
                </DropdownMenu.Trigger>
                <DropdownMenu.Portal>
                  <DropdownMenu.Content className="user-menu" align="end" sideOffset={8}>
                    <DropdownMenu.Label className="user-menu__label">
                      <strong>{user?.displayName ?? user?.login}</strong>
                      <span>{user?.accessLabel ?? user?.role}</span>
                    </DropdownMenu.Label>
                    <DropdownMenu.Separator className="user-menu__separator" />
                    {isAdmin && (
                      <DropdownMenu.Item
                        className="user-menu__item"
                        onSelect={() => {
                          void navigate('/admin/users')
                        }}
                      >
                        <UserRound size={16} />
                        {t('nav.users')}
                      </DropdownMenu.Item>
                    )}
                    <DropdownMenu.Item
                      className="user-menu__item"
                      onSelect={() => prefs.setTheme(prefs.theme === 'dark' ? 'light' : 'dark')}
                    >
                      {prefs.theme === 'dark' ? <Sun size={16} /> : <Moon size={16} />}
                      {t('common.theme')}
                    </DropdownMenu.Item>
                    <DropdownMenu.Item
                      className="user-menu__item"
                      onSelect={() => prefs.setLocale(prefs.locale === 'en' ? 'ru' : 'en')}
                    >
                      <Languages size={16} />
                      {t('common.language')}
                    </DropdownMenu.Item>
                    <DropdownMenu.Item
                      className="user-menu__item"
                      disabled={!telemetryAvailable}
                      onSelect={() =>
                        void requestTelemetryConsent(telemetryEnabled ? 'denied' : 'granted')
                      }
                    >
                      <BarChart3 size={16} />
                      {telemetryLabel}
                    </DropdownMenu.Item>
                    <DropdownMenu.Separator className="user-menu__separator" />
                    <DropdownMenu.Item
                      className="user-menu__item user-menu__item--danger"
                      onSelect={() => {
                        void logout()
                      }}
                    >
                      <LogOut size={16} />
                      {t('shell.logout')}
                    </DropdownMenu.Item>
                  </DropdownMenu.Content>
                </DropdownMenu.Portal>
              </DropdownMenu.Root>
            </div>
          </header>
          {sessionError && (
            <output className="degraded-banner">
              <AlertTriangle size={15} />
              <span>{sessionError}</span>
            </output>
          )}
          <main id="main-content" className="page-content" tabIndex={-1}>
            <div className="page-content__inner">
              <ScopeSwitcherContext.Provider value={scopeSwitcherControl}>
                {children}
              </ScopeSwitcherContext.Provider>
            </div>
          </main>
          <footer className="shell-footer">
            <div className="shell-footer__version" aria-live="polite">
              <a className="shell-footer__tag" href={RELEASES_URL} target="_blank" rel="noreferrer">
                {BUILD_VERSION_TAG}
                <ExternalLink size={12} aria-hidden="true" />
              </a>
              {!release.isError && release.data?.status === 'update_available' && latestVersion && (
                <a
                  href={releaseDestination(release.data.releaseUrl)}
                  target="_blank"
                  rel="noreferrer"
                >
                  <Badge tone="ready">
                    {t('shell.update', { version: formatVersionTag(latestVersion) })}
                  </Badge>
                </a>
              )}
            </div>
            <a href="/swagger.html">{t('shell.apiDocs')}</a>
          </footer>
        </div>
        <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
        <ScopeSwitcher open={scopeOpen} onOpenChange={setScopeOpen} />
      </div>
    </Dialog.Root>
  )
}
