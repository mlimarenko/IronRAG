import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useNavigate, useLocation, Link } from 'react-router-dom';
import { useApp } from '@/shared/contexts/app-context';
import { useCan } from '@/shared/auth/useCan';
import { onShellIntent } from '@/shared/lib/shell-events';
import { CommandPaletteMount } from '@/features/command-palette/CommandPaletteMount';
import { adminApi, ASYNC_OPERATION_TERMINAL_STATES, Catalog, Ops, unwrap } from '@/shared/api';
import { ShellFooter } from '@/app/components/ShellFooter';
import { UserMenu } from '@/app/components/UserMenu';
import { ADMIN_SECTIONS, ADMIN_SECTION_GROUPS } from '@/features/admin/model/sections';
import {
  Home, FileText, Share2, MessageSquare,
  ChevronDown, Menu, X, Plus, Trash2, AlertTriangle, Search, Building2, Library as LibraryIcon,
} from 'lucide-react';
import { Button } from '@/shared/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/shared/components/ui/dropdown-menu';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/shared/components/ui/dialog';
import { Input } from '@/shared/components/ui/input';
import { Label } from '@/shared/components/ui/label';
import { errorMessage } from '@/shared/lib/errorMessage';

// Primary nav — the four daily-operator surfaces every authenticated role
// sees. Admin and Swagger are deliberately NOT here: Admin is a role-gated
// single entry rendered separately, Swagger is demoted to the footer.
const PRIMARY_NAV = [
  { id: 'home', path: '/dashboard', icon: Home },
  { id: 'documents', path: '/documents', icon: FileText },
  { id: 'graph', path: '/graph', icon: Share2 },
  { id: 'assistant', path: '/assistant', icon: MessageSquare },
] as const;

const CATALOG_DELETE_POLL_INTERVAL_MS = 2000;

function delay(ms: number) {
  return new Promise(resolve => {
    window.setTimeout(resolve, ms);
  });
}

export function AppShell({ children }: { children: React.ReactNode }) {
  const { t } = useTranslation();
  const {
    workspaces, activeWorkspace, libraries, activeLibrary,
    setWorkspaces, setActiveWorkspace, setLibraries, setActiveLibrary,
    refreshSession,
  } = useApp();
  const { can } = useCan();
  const navigate = useNavigate();
  const location = useLocation();
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try { return localStorage.getItem('ironrag.sidebarCollapsed') === '1'; } catch { return false; }
  });
  const toggleSidebar = () =>
    setSidebarCollapsed((prev) => {
      const next = !prev;
      try { localStorage.setItem('ironrag.sidebarCollapsed', next ? '1' : '0'); } catch { /* ignore */ }
      return next;
    });

  const [createWsOpen, setCreateWsOpen] = useState(false);
  const [createLibOpen, setCreateLibOpen] = useState(false);
  const [deleteWsOpen, setDeleteWsOpen] = useState(false);
  const [deleteLibOpen, setDeleteLibOpen] = useState(false);
  const [newWsName, setNewWsName] = useState('');
  const [newLibName, setNewLibName] = useState('');
  const [deleteConfirmName, setDeleteConfirmName] = useState('');
  const [deleteSubmitting, setDeleteSubmitting] = useState(false);
  const [workspaceSearch, setWorkspaceSearch] = useState('');
  const [librarySearch, setLibrarySearch] = useState('');
  // Controlled so the dashboard empty state can pop the picker via a shell
  // intent (the desktop selector; mobile falls back to the create dialog).
  const [libraryMenuOpen, setLibraryMenuOpen] = useState(false);

  const canManageWorkspace = can('workspace.manage');
  const canCreateLibrary = can('library.create');
  const canDeleteLibrary = can('library.delete');
  const canAccessAdmin = can('admin.access');

  // Surgical bridge for cross-surface intents (dashboard empty-state CTAs).
  useEffect(() => onShellIntent('open-library-picker', () => setLibraryMenuOpen(true)), []);
  useEffect(() => {
    if (!canCreateLibrary) return undefined;
    return onShellIntent('create-library', () => setCreateLibOpen(true));
  }, [canCreateLibrary]);

  const isActive = (path: string) => location.pathname.startsWith(path);
  const workspaceSearchValue = workspaceSearch.trim().toLowerCase();
  const librarySearchValue = librarySearch.trim().toLowerCase();
  const filteredWorkspaces = useMemo(
    () =>
      workspaceSearchValue
        ? workspaces.filter((workspace) =>
            workspace.name.toLowerCase().includes(workspaceSearchValue),
          )
        : workspaces,
    [workspaceSearchValue, workspaces],
  );
  const filteredLibraries = useMemo(
    () =>
      librarySearchValue
        ? libraries.filter((library) =>
            library.name.toLowerCase().includes(librarySearchValue),
          )
        : libraries,
    [libraries, librarySearchValue],
  );

  const handleCreateWorkspace = async () => {
    if (!newWsName.trim()) return;
    try {
      await adminApi.createWorkspace(newWsName.trim());
      toast.success(t('shell.workspaceCreated'));
      await refreshSession();
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('shell.workspaceCreateFailed')));
    }
    setNewWsName('');
    setCreateWsOpen(false);
  };

  const handleCreateLibrary = async () => {
    if (!newLibName.trim() || !activeWorkspace) return;
    try {
      await adminApi.createLibrary(activeWorkspace.id, newLibName.trim());
      toast.success(t('shell.libraryCreated'));
      await refreshSession();
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('shell.libraryCreateFailed')));
    }
    setNewLibName('');
    setCreateLibOpen(false);
  };

  const handleDeleteWorkspace = async () => {
    if (!activeWorkspace || deleteConfirmName !== activeWorkspace.name || deleteSubmitting) return;
    const workspace = activeWorkspace;
    setDeleteSubmitting(true);
    try {
      const admission = unwrap(
        await Catalog.deleteCatalogWorkspace({ path: { workspaceId: workspace.id } }),
      );
      setDeleteConfirmName('');
      setDeleteWsOpen(false);
      setWorkspaces(prev => prev.filter(item => item.id !== workspace.id));
      setLibraries(prev => prev.filter(item => item.workspaceId !== workspace.id));
      setActiveWorkspace(null);
      setActiveLibrary(null);
      const toastId = toast.loading(t('shell.workspaceDeletionStarted', { name: workspace.name }));
      void pollCatalogDeletion(
        admission.operationId,
        toastId,
        t('shell.workspaceDeleted'),
        t('shell.workspaceDeleteFailed'),
      );
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('shell.workspaceDeleteFailed')));
    } finally {
      setDeleteSubmitting(false);
    }
  };

  const handleDeleteLibrary = async () => {
    if (!activeLibrary || deleteConfirmName !== activeLibrary.name || !activeWorkspace || deleteSubmitting) return;
    const workspace = activeWorkspace;
    const library = activeLibrary;
    setDeleteSubmitting(true);
    try {
      const admission = unwrap(
        await Catalog.deleteCatalogLibrary({
          path: { workspaceId: workspace.id, libraryId: library.id },
        }),
      );
      setDeleteConfirmName('');
      setDeleteLibOpen(false);
      setLibraries(prev => prev.filter(item => item.id !== library.id));
      setActiveLibrary(null);
      const toastId = toast.loading(t('shell.libraryDeletionStarted', { name: library.name }));
      void pollCatalogDeletion(
        admission.operationId,
        toastId,
        t('shell.libraryDeleted'),
        t('shell.libraryDeleteFailed'),
      );
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('shell.libraryDeleteFailed')));
    } finally {
      setDeleteSubmitting(false);
    }
  };

  const pollCatalogDeletion = async (
    operationId: string,
    toastId: string | number,
    successMessage: string,
    failureMessage: string,
  ) => {
    try {
      for (;;) {
        await delay(CATALOG_DELETE_POLL_INTERVAL_MS);
        const operation = unwrap(await Ops.getAsyncOperation({ path: { operationId } }));
        if (!ASYNC_OPERATION_TERMINAL_STATES.has(operation.status)) continue;
        if (operation.status === 'ready') {
          toast.success(successMessage, { id: toastId });
        } else {
          toast.error(failureMessage, { id: toastId });
        }
        await refreshSession();
        return;
      }
    } catch (err: unknown) {
      toast.error(errorMessage(err, failureMessage), { id: toastId });
      try {
        await refreshSession();
      } catch {
        // Keep the original polling error visible; the next navigation/session refresh reconciles state.
      }
    }
  };

  const missingPurposes = activeLibrary?.missingBindingPurposes ?? [];
  const showAiWarning = canAccessAdmin && !!activeLibrary && missingPurposes.length > 0;
  const selectorContentClass =
    'w-[min(22rem,calc(100vw-2rem))] max-h-[min(32rem,calc(100vh-5rem))] overflow-hidden p-0';
  const selectorListClass = 'max-h-[min(22rem,calc(100vh-13rem))] overflow-y-auto p-1';

  const renderWorkspaceMenu = (align: 'start' | 'end', collapsed = false) => (
    <DropdownMenuContent
      align={align}
      {...(collapsed ? { side: 'right' as const } : {})}
      className={selectorContentClass}
    >
      <div className="border-b p-2">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={workspaceSearch}
            onChange={(event) => setWorkspaceSearch(event.target.value)}
            onKeyDown={(event) => event.stopPropagation()}
            placeholder={t('shell.searchWorkspaces')}
            className="h-8 pl-8 text-xs"
          />
        </div>
      </div>
      <div className={selectorListClass}>
        {filteredWorkspaces.length === 0 ? (
          <div className="px-2 py-3 text-xs text-muted-foreground">
            {t('shell.noWorkspaceMatches')}
          </div>
        ) : (
          filteredWorkspaces.map(ws => (
            <DropdownMenuItem key={ws.id} onClick={() => setActiveWorkspace(ws)} title={ws.name}>
              <span className="truncate">{ws.name}</span>
            </DropdownMenuItem>
          ))
        )}
      </div>
      {canManageWorkspace && (
        <>
          <DropdownMenuSeparator />
          <div className="p-1">
            <DropdownMenuItem onClick={() => setCreateWsOpen(true)}>
              <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createWorkspace')}
            </DropdownMenuItem>
            {activeWorkspace && (
              <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteWsOpen(true); }} className="text-destructive">
                <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteWorkspace')}
              </DropdownMenuItem>
            )}
          </div>
        </>
      )}
    </DropdownMenuContent>
  );

  const renderLibraryMenu = (align: 'start' | 'end', collapsed = false) => (
    <DropdownMenuContent
      align={align}
      {...(collapsed ? { side: 'right' as const } : {})}
      className={selectorContentClass}
    >
      <div className="border-b p-2">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={librarySearch}
            onChange={(event) => setLibrarySearch(event.target.value)}
            onKeyDown={(event) => event.stopPropagation()}
            placeholder={t('shell.searchLibraries')}
            className="h-8 pl-8 text-xs"
          />
        </div>
      </div>
      <div className={selectorListClass}>
        {filteredLibraries.length === 0 ? (
          <div className="px-2 py-3 text-xs text-muted-foreground">
            {t('shell.noLibraryMatches')}
          </div>
        ) : (
          filteredLibraries.map(lib => (
            <DropdownMenuItem key={lib.id} onClick={() => setActiveLibrary(lib)} title={lib.name}>
              <span className="truncate">{lib.name}</span>
            </DropdownMenuItem>
          ))
        )}
      </div>
      {(canCreateLibrary || canDeleteLibrary) && (
        <>
          <DropdownMenuSeparator />
          <div className="p-1">
            {canCreateLibrary && (
              <DropdownMenuItem onClick={() => setCreateLibOpen(true)}>
                <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createLibrary')}
              </DropdownMenuItem>
            )}
            {canDeleteLibrary && activeLibrary && (
              <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteLibOpen(true); }} className="text-destructive">
                <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteLibrary')}
              </DropdownMenuItem>
            )}
          </div>
        </>
      )}
    </DropdownMenuContent>
  );

  // Labelled scope selector trigger — replaces the two identical unlabelled
  // buttons. A leading icon + caption ("Workspace" / "Library") makes the
  // current scope unambiguous at a glance.
  const renderScopeTrigger = (
    caption: string,
    value: string,
    Icon: typeof Building2,
    options: { fullWidth?: boolean; collapsed?: boolean } = {},
  ) => {
    if (options.collapsed) {
      return (
        <button
          type="button"
          className="flex w-full items-center justify-center rounded-lg border border-shell-border bg-shell-hover px-0 py-2 outline-none transition-colors hover:bg-shell-active/15 focus-visible:ring-2 focus-visible:ring-shell-active/60"
          title={value}
        >
          <Icon className="h-3.5 w-3.5 shrink-0 text-shell-muted" />
        </button>
      );
    }
    return (
      <button
        type="button"
        className={`flex items-center gap-2 rounded-lg border border-shell-border bg-shell-hover text-left outline-none transition-colors hover:bg-shell-active/15 focus-visible:ring-2 focus-visible:ring-shell-active/60 ${
          options.fullWidth ? 'w-full px-3 py-2' : 'px-2.5 py-1'
        }`}
      >
        <Icon className="h-3.5 w-3.5 shrink-0 text-shell-muted" />
        <span className="flex min-w-0 flex-1 flex-col leading-tight">
          <span className="section-label text-shell-muted">
            {caption}
          </span>
          <span
            className={`min-w-0 truncate text-xs font-medium text-shell-foreground ${
              options.fullWidth ? 'max-w-none' : 'max-w-[120px]'
            }`}
            title={value}
          >
            {value}
          </span>
        </span>
        <ChevronDown className="h-3.5 w-3.5 shrink-0 opacity-50" />
      </button>
    );
  };

  const aiWarningButton = (fullWidth = false, collapsed = false) => (
    <button
      type="button"
      onClick={() => { void navigate('/admin/ai'); setMobileMenuOpen(false); }}
      className={`status-warning flex items-center gap-1.5 rounded-full px-2.5 py-1 text-2xs font-semibold transition-colors hover:bg-status-warning/15 ${fullWidth ? 'w-full justify-center' : ''} ${collapsed ? 'px-1.5' : ''}`}
      title={t('shell.configureInSettings')}
    >
      <AlertTriangle className="h-3.5 w-3.5" />
      {!collapsed && <span>{t('admin.bindingsMissing', { count: missingPurposes.length })}</span>}
    </button>
  );

  const visibleAdminSections = ADMIN_SECTIONS.filter((section) => can(section.capability));
  const isAdminRoute = location.pathname.startsWith('/admin');
  const currentAdminSegment = isAdminRoute ? (location.pathname.split('/')[2] ?? 'libraries') : null;

  const sidebarLinkClass = (active: boolean, nested = false, collapsed = false) =>
    `flex min-w-0 items-center gap-2 rounded-lg py-2 text-sm font-semibold transition-colors ${
      collapsed ? 'justify-center px-0' : 'px-3'
    } ${
      nested ? (collapsed ? 'text-xs' : 'pl-9 text-xs') : ''
    } ${
      active
        ? 'bg-shell-active/20 text-shell-foreground ring-1 ring-shell-active/35'
        : 'text-shell-muted hover:bg-shell-hover hover:text-shell-foreground'
    }`;

  const renderSidebarContent = (mobile = false, collapsed = false) => (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <div
        className={`flex h-14 shrink-0 items-center gap-2.5 border-b border-shell-border ${
          collapsed ? 'justify-center px-0' : 'px-4'
        }`}
      >
        {mobile ? (
          <Link
            to="/dashboard"
            onClick={() => setMobileMenuOpen(false)}
            className="group flex min-w-0 items-center gap-2.5 text-sm font-bold tracking-tight text-shell-foreground"
          >
            <img
              src="/favicon.svg"
              alt=""
              aria-hidden="true"
              className="h-6 w-auto shrink-0 transition-transform duration-200 group-hover:scale-110"
            />
            <span className="truncate">{t('common.productName')}</span>
          </Link>
        ) : (
          <button
            type="button"
            onClick={toggleSidebar}
            title={collapsed ? t('shell.expandSidebar') : t('shell.collapseSidebar')}
            aria-label={collapsed ? t('shell.expandSidebar') : t('shell.collapseSidebar')}
            className="group flex min-w-0 items-center gap-2.5 text-sm font-bold tracking-tight text-shell-foreground"
          >
            <img
              src="/favicon.svg"
              alt=""
              aria-hidden="true"
              className="h-6 w-auto shrink-0 transition-transform duration-200 group-hover:scale-110"
            />
            {!collapsed && <span className="truncate">{t('common.productName')}</span>}
          </button>
        )}
      </div>

      <div className="space-y-2 border-b border-shell-border p-3">
        {showAiWarning && aiWarningButton(true, collapsed)}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            {renderScopeTrigger(
              t('shell.workspaceScope'),
              activeWorkspace?.name ?? t('shell.noWorkspace'),
              Building2,
              { fullWidth: true, collapsed },
            )}
          </DropdownMenuTrigger>
          {renderWorkspaceMenu('start', collapsed)}
        </DropdownMenu>

        <DropdownMenu open={libraryMenuOpen} onOpenChange={setLibraryMenuOpen}>
          <DropdownMenuTrigger asChild>
            {renderScopeTrigger(
              t('shell.libraryScope'),
              activeLibrary?.name ?? t('shell.noLibrary'),
              LibraryIcon,
              { fullWidth: true, collapsed },
            )}
          </DropdownMenuTrigger>
          {renderLibraryMenu('start', collapsed)}
        </DropdownMenu>
      </div>

      <nav aria-label={t('shell.primaryNav')} className="min-h-0 flex-1 space-y-4 overflow-y-auto px-3 py-4">
        <div className="space-y-1">
          {PRIMARY_NAV.map(item => (
            <Link
              key={item.path}
              to={item.path}
              onClick={() => mobile && setMobileMenuOpen(false)}
              aria-current={isActive(item.path) ? 'page' : undefined}
              className={sidebarLinkClass(isActive(item.path), false, collapsed)}
              title={collapsed ? t(`nav.${item.id}`) : undefined}
            >
              <item.icon className="h-4 w-4 shrink-0" />
              {!collapsed && <span className="truncate">{t(`nav.${item.id}`)}</span>}
            </Link>
          ))}
        </div>

        {canAccessAdmin && (
          <div className="space-y-3 border-t border-shell-border pt-4">
            {!collapsed && (
              <div className="px-3 section-label text-shell-muted">
                {t('nav.admin')}
              </div>
            )}
            {ADMIN_SECTION_GROUPS.map((group) => {
              const sections = visibleAdminSections.filter((section) => section.group === group);
              if (sections.length === 0) return null;
              return (
                <div key={group} className="space-y-1">
                  {!collapsed && (
                    <div className="px-3 text-2xs font-semibold uppercase tracking-caps text-shell-muted/70">
                      {t(`admin.nav.group.${group}`)}
                    </div>
                  )}
                  {sections.map((section) => {
                    const Icon = section.icon;
                    const active =
                      currentAdminSegment === section.segment ||
                      (currentAdminSegment != null &&
                        (section.alsoMatch?.includes(currentAdminSegment) ?? false));
                    return (
                      <Link
                        key={section.segment}
                        to={`/admin/${section.segment}`}
                        onClick={() => mobile && setMobileMenuOpen(false)}
                        aria-current={active ? 'page' : undefined}
                        className={sidebarLinkClass(active, true, collapsed)}
                        title={collapsed ? t(`admin.nav.${section.labelKey}`) : undefined}
                      >
                        <Icon className="h-3.5 w-3.5 shrink-0" />
                        {!collapsed && <span className="truncate">{t(`admin.nav.${section.labelKey}`)}</span>}
                      </Link>
                    );
                  })}
                </div>
              );
            })}
          </div>
        )}
      </nav>

      <div className="shrink-0 border-t border-shell-border p-3">
        <UserMenu variant="sidebar" onAfterAction={() => setMobileMenuOpen(false)} collapsed={collapsed} />
      </div>
    </div>
  );

  return (
    <div className="flex h-screen max-h-screen overflow-hidden bg-surface-sunken">
      <aside
        className={`hidden shrink-0 border-r border-shell-border bg-shell transition-[width] duration-200 md:flex ${
          sidebarCollapsed ? 'w-16' : 'w-64'
        }`}
      >
        {renderSidebarContent(false, sidebarCollapsed)}
      </aside>

      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        <header className="flex h-13 shrink-0 items-center gap-2 border-b border-border bg-background px-3 sm:px-4 md:hidden">
          <button
            type="button"
            className="-ml-1 rounded-lg p-1.5 text-foreground transition-colors hover:bg-muted"
            onClick={() => setMobileMenuOpen(true)}
            aria-label={t('shell.toggleNavigation')}
            aria-expanded={mobileMenuOpen}
          >
            <Menu className="h-5 w-5" />
          </button>
        </header>

        {mobileMenuOpen && (
          <>
            <button
              type="button"
              className="fixed inset-0 z-40 bg-foreground/35 backdrop-blur-[1px] md:hidden"
              aria-label={t('shell.toggleNavigation')}
              onClick={() => setMobileMenuOpen(false)}
            />
            <aside
              className="fixed inset-y-0 left-0 z-50 flex w-[min(20rem,88vw)] border-r border-shell-border bg-shell shadow-overlay md:hidden"
            >
              <button
                type="button"
                className="absolute right-2 top-2 z-10 rounded-lg p-1.5 text-shell-muted transition-colors hover:bg-shell-hover hover:text-shell-foreground"
                aria-label={t('shell.toggleNavigation')}
                onClick={() => setMobileMenuOpen(false)}
              >
                <X className="h-4 w-4" />
              </button>
              {renderSidebarContent(true, false)}
            </aside>
          </>
        )}

        <main className="flex min-h-0 flex-1 flex-col overflow-hidden">
          {children}
        </main>

        {/* Footer (hosts the demoted Swagger link) */}
        <ShellFooter />
      </div>

      {/* Global ⌘K command palette — self-contained (owns its own open state
          + keyboard shortcut), so this mount stays a single element. */}
      <CommandPaletteMount />

      {/* Dialogs */}
      <Dialog open={createWsOpen} onOpenChange={setCreateWsOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('shell.createWorkspaceTitle')}</DialogTitle>
            <DialogDescription>{t('shell.createWorkspaceDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div>
              <Label htmlFor="ws-name">{t('shell.workspaceName')}</Label>
              <Input id="ws-name" value={newWsName} onChange={e => setNewWsName(e.target.value)} placeholder={t('shell.workspaceNamePlaceholder')} className="mt-1.5" />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateWsOpen(false)}>{t('shell.cancel')}</Button>
            <Button onClick={handleCreateWorkspace} disabled={!newWsName.trim()}>{t('shell.create')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createLibOpen} onOpenChange={setCreateLibOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('shell.createLibraryTitle')}</DialogTitle>
            <DialogDescription>{t('shell.createLibraryDesc', { name: activeWorkspace?.name })}</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div>
              <Label htmlFor="lib-name">{t('shell.libraryName')}</Label>
              <Input id="lib-name" value={newLibName} onChange={e => setNewLibName(e.target.value)} placeholder={t('shell.libraryNamePlaceholder')} className="mt-1.5" />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateLibOpen(false)}>{t('shell.cancel')}</Button>
            <Button onClick={handleCreateLibrary} disabled={!newLibName.trim()}>{t('shell.create')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={deleteWsOpen} onOpenChange={setDeleteWsOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('shell.deleteWorkspaceTitle')}</DialogTitle>
            <DialogDescription>{t('shell.deleteWorkspaceDesc', { name: activeWorkspace?.name })}</DialogDescription>
          </DialogHeader>
          <div>
            <Label htmlFor="del-ws-confirm">{t('shell.typeToConfirm', { name: activeWorkspace?.name })}</Label>
            <Input id="del-ws-confirm" value={deleteConfirmName} onChange={e => setDeleteConfirmName(e.target.value)} className="mt-1.5" />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteWsOpen(false)} disabled={deleteSubmitting}>{t('shell.cancel')}</Button>
            <Button variant="destructive" onClick={handleDeleteWorkspace} disabled={deleteConfirmName !== activeWorkspace?.name || deleteSubmitting}>{t('shell.delete')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={deleteLibOpen} onOpenChange={setDeleteLibOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('shell.deleteLibraryTitle')}</DialogTitle>
            <DialogDescription>{t('shell.deleteLibraryDesc', { name: activeLibrary?.name })}</DialogDescription>
          </DialogHeader>
          <div>
            <Label htmlFor="del-lib-confirm">{t('shell.typeToConfirm', { name: activeLibrary?.name })}</Label>
            <Input id="del-lib-confirm" value={deleteConfirmName} onChange={e => setDeleteConfirmName(e.target.value)} className="mt-1.5" />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteLibOpen(false)} disabled={deleteSubmitting}>{t('shell.cancel')}</Button>
            <Button variant="destructive" onClick={handleDeleteLibrary} disabled={deleteConfirmName !== activeLibrary?.name || deleteSubmitting}>{t('shell.delete')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
