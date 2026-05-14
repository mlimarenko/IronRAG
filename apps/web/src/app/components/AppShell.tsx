import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useNavigate, useLocation, Link } from 'react-router-dom';
import { useApp } from '@/shared/contexts/app-context';
import { adminApi, ASYNC_OPERATION_TERMINAL_STATES, Catalog, Ops, unwrap } from '@/shared/api';
import { ShellFooter } from '@/app/components/ShellFooter';
import {
  Home, FileText, Share2, MessageSquare, Settings, Code2,
  ChevronDown, LogOut, Menu, X, Plus, Trash2, AlertTriangle, Search
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

const NAV_ITEMS = [
  { id: 'home', path: '/dashboard', icon: Home },
  { id: 'documents', path: '/documents', icon: FileText },
  { id: 'graph', path: '/graph', icon: Share2 },
  { id: 'assistant', path: '/assistant', icon: MessageSquare },
  { id: 'admin', path: '/admin', icon: Settings },
  { id: 'swagger', path: '/swagger', icon: Code2 },
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
    user, workspaces, activeWorkspace, libraries, activeLibrary,
    setWorkspaces, setActiveWorkspace, setLibraries, setActiveLibrary, logout,
    refreshSession
  } = useApp();
  const navigate = useNavigate();
  const location = useLocation();
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);

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
  const shellUserName = user?.displayName ?? t('shell.userFallback');
  const shellAccessLabel = user?.accessLabel ?? t('shell.accessFallback');

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
  const selectorContentClass =
    'w-[min(22rem,calc(100vw-2rem))] max-h-[min(32rem,calc(100vh-5rem))] overflow-hidden p-0';
  const selectorListClass = 'max-h-[min(22rem,calc(100vh-13rem))] overflow-y-auto p-1';

  const renderWorkspaceMenu = (align: 'start' | 'end') => (
    <DropdownMenuContent align={align} className={selectorContentClass}>
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
            <DropdownMenuItem key={ws.id} onClick={() => setActiveWorkspace(ws)}>
              <span className="truncate">{ws.name}</span>
            </DropdownMenuItem>
          ))
        )}
      </div>
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
    </DropdownMenuContent>
  );

  const renderLibraryMenu = (align: 'start' | 'end') => (
    <DropdownMenuContent align={align} className={selectorContentClass}>
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
            <DropdownMenuItem key={lib.id} onClick={() => setActiveLibrary(lib)}>
              <span className="truncate">{lib.name}</span>
            </DropdownMenuItem>
          ))
        )}
      </div>
      <DropdownMenuSeparator />
      <div className="p-1">
        <DropdownMenuItem onClick={() => setCreateLibOpen(true)}>
          <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createLibrary')}
        </DropdownMenuItem>
        {activeLibrary && (
          <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteLibOpen(true); }} className="text-destructive">
            <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteLibrary')}
          </DropdownMenuItem>
        )}
      </div>
    </DropdownMenuContent>
  );

  return (
    <div className="h-screen max-h-screen flex flex-col overflow-hidden bg-background">
      {/* Top shell */}
      <header className="h-13 flex items-center px-4 gap-2 shrink-0 relative z-50" style={{
        background: 'linear-gradient(180deg, hsl(var(--shell-bg)), hsl(225 32% 8%))',
        borderBottom: '1px solid hsl(var(--shell-border))',
        boxShadow: '0 1px 3px hsl(225 32% 4% / 0.3)',
      }}>
        {/* Brand */}
        <Link to="/dashboard" className="font-bold text-sm tracking-tight mr-4 flex items-center gap-2.5 group" style={{ color: 'hsl(var(--shell-foreground))' }}>
          <img
            src="/favicon.svg"
            alt=""
            aria-hidden="true"
            className="h-6 w-auto shrink-0 transition-transform duration-200 group-hover:scale-110"
          />
          <span className="hidden sm:inline">{t('common.productName')}</span>
        </Link>

        {/* Desktop nav */}
        <nav className="hidden md:flex items-center gap-0.5 mr-auto">
          {NAV_ITEMS.map(item => (
            <button
              key={item.path}
              onClick={() => navigate(item.path)}
              className={`shell-nav-item flex items-center gap-1.5 ${isActive(item.path) ? 'active' : ''}`}
            >
              <item.icon className="h-3.5 w-3.5" />
              <span>{t(`nav.${item.id}`)}</span>
            </button>
          ))}
        </nav>

        {/* Mobile menu toggle */}
        <button
          className="md:hidden ml-auto p-1.5 rounded-lg transition-colors"
          onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
          style={{ color: 'hsl(var(--shell-foreground))' }}
          aria-label={t('shell.toggleNavigation')}
        >
          {mobileMenuOpen ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
        </button>

        {/* Right side controls */}
        <div className="hidden md:flex items-center gap-1.5 ml-auto">
          {/* Library readiness warning */}
          {activeLibrary && missingPurposes.length > 0 && (
            <button
              onClick={() => navigate('/admin?tab=ai')}
              className="flex items-center gap-1 rounded-full border border-amber-700/25 bg-amber-50 px-2.5 py-1 text-[11px] font-semibold text-amber-900 hover:bg-amber-100 transition-colors dark:border-amber-400/25 dark:bg-amber-950/40 dark:text-amber-200 dark:hover:bg-amber-950/60"
              title={t('shell.configureInSettings')}
            >
              <AlertTriangle className="h-3 w-3" />
              <span>{missingPurposes.length > 1 ? t('admin.bindingsMissingPlural') : t('admin.bindingsMissing')}</span>
            </button>
          )}

          {/* Workspace selector */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button className="flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg transition-all duration-200 font-medium" style={{
                color: 'hsl(var(--shell-foreground))',
                background: 'hsl(var(--shell-hover))',
                border: '1px solid hsl(var(--shell-border))',
              }}>
                <span className="truncate max-w-[100px]">{activeWorkspace?.name ?? t('shell.noWorkspace')}</span>
                <ChevronDown className="h-3 w-3 opacity-50" />
              </button>
            </DropdownMenuTrigger>
            {renderWorkspaceMenu('end')}
          </DropdownMenu>

          {/* Library selector */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button className="flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg transition-all duration-200 font-medium" style={{
                color: 'hsl(var(--shell-foreground))',
                background: 'hsl(var(--shell-hover))',
                border: '1px solid hsl(var(--shell-border))',
              }}>
                <span className="truncate max-w-[100px]">{activeLibrary?.name ?? t('shell.noLibrary')}</span>
                <ChevronDown className="h-3 w-3 opacity-50" />
              </button>
            </DropdownMenuTrigger>
            {renderLibraryMenu('end')}
          </DropdownMenu>

          {/* User menu */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button className="flex items-center gap-2 text-xs px-2.5 py-1.5 rounded-lg transition-all duration-200" style={{
                color: 'hsl(var(--shell-foreground))',
                background: 'hsl(var(--shell-hover))',
                border: '1px solid hsl(var(--shell-border))',
              }}>
                <div className="w-5 h-5 rounded-full flex items-center justify-center text-[10px] font-bold" style={{
                  background: 'linear-gradient(135deg, hsl(var(--shell-active) / 0.3), hsl(var(--shell-active) / 0.15))',
                  color: 'hsl(var(--shell-active))',
                }}>
                  {shellUserName[0].toUpperCase()}
                </div>
                <span className="truncate max-w-[80px] font-medium">{shellUserName}</span>
                <ChevronDown className="h-3 w-3 opacity-50" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-[180px]">
              <div className="px-2 py-1.5 text-xs text-muted-foreground font-medium">
                {shellAccessLabel}
              </div>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => { void logout(); void navigate('/login'); }}>
                <LogOut className="h-3.5 w-3.5 mr-1.5" /> {t('shell.logout')}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      {/* Mobile nav overlay */}
      {mobileMenuOpen && (
        <div className="md:hidden border-b p-3 space-y-1 animate-fade-in" style={{ background: 'hsl(var(--shell-bg))' }}>
          {NAV_ITEMS.map(item => (
            <button
              key={item.path}
              onClick={() => { void navigate(item.path); setMobileMenuOpen(false); }}
              className={`shell-nav-item flex items-center gap-2 w-full ${isActive(item.path) ? 'active' : ''}`}
            >
              <item.icon className="h-4 w-4" />
              <span>{t(`nav.${item.id}`)}</span>
            </button>
          ))}
          <div className="pt-2 flex flex-wrap gap-2">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button className="flex items-center gap-1.5 text-xs px-2.5 py-1.5 rounded-lg font-medium" style={{ color: 'hsl(var(--shell-foreground))', background: 'hsl(var(--shell-hover))', border: '1px solid hsl(var(--shell-border))' }}>
                  <span className="truncate max-w-[120px]">{activeWorkspace?.name ?? t('shell.noWorkspace')}</span>
                  <ChevronDown className="h-3 w-3 opacity-50" />
                </button>
              </DropdownMenuTrigger>
              {renderWorkspaceMenu('start')}
            </DropdownMenu>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button className="flex items-center gap-1.5 text-xs px-2.5 py-1.5 rounded-lg font-medium" style={{ color: 'hsl(var(--shell-foreground))', background: 'hsl(var(--shell-hover))', border: '1px solid hsl(var(--shell-border))' }}>
                  <span className="truncate max-w-[120px]">{activeLibrary?.name ?? t('shell.noLibrary')}</span>
                  <ChevronDown className="h-3 w-3 opacity-50" />
                </button>
              </DropdownMenuTrigger>
              {renderLibraryMenu('start')}
            </DropdownMenu>
          </div>
        </div>
      )}

      {/* Main content */}
      <main className="flex-1 min-h-0 flex flex-col overflow-hidden">
        {children}
      </main>

      {/* Footer */}
      <ShellFooter />

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
