import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useNavigate, useLocation, Link } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { adminApi, apiFetch } from '@/api';
import { BUILD_VERSION_LABEL } from '@/lib/build-version';
import {
  Home, FileText, Share2, MessageSquare, Settings, Code2,
  ChevronDown, Globe, LogOut, Menu, X, Plus, Trash2, AlertTriangle
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';

const NAV_KEYS = [
  { key: 'nav.home', path: '/dashboard', icon: Home },
  { key: 'nav.documents', path: '/documents', icon: FileText },
  { key: 'nav.graph', path: '/graph', icon: Share2 },
  { key: 'nav.assistant', path: '/assistant', icon: MessageSquare },
  { key: 'nav.admin', path: '/admin', icon: Settings },
  { key: 'nav.swagger', path: '/swagger', icon: Code2 },
];

export function AppShell({ children }: { children: React.ReactNode }) {
  const { t } = useTranslation();
  const {
    user, workspaces, activeWorkspace, libraries, activeLibrary,
    setActiveWorkspace, setActiveLibrary, logout,
    setWorkspaces, setLibraries, refreshSession
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

  const isActive = (path: string) => location.pathname.startsWith(path);

  const handleCreateWorkspace = async () => {
    if (!newWsName.trim()) return;
    try {
      await adminApi.createWorkspace(newWsName.trim());
      toast.success(t('shell.workspaceCreated'));
      await refreshSession();
    } catch (err: any) {
      toast.error(err?.message || 'Failed to create workspace');
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
    } catch (err: any) {
      toast.error(err?.message || 'Failed to create library');
    }
    setNewLibName('');
    setCreateLibOpen(false);
  };

  const handleDeleteWorkspace = async () => {
    if (!activeWorkspace || deleteConfirmName !== activeWorkspace.name) return;
    try {
      await apiFetch(`/catalog/workspaces/${activeWorkspace.id}`, { method: 'DELETE' });
      toast.success(t('shell.workspaceDeleted'));
      await refreshSession();
    } catch (err: any) {
      toast.error(err?.message || 'Failed to delete workspace');
    }
    setDeleteConfirmName('');
    setDeleteWsOpen(false);
  };

  const handleDeleteLibrary = async () => {
    if (!activeLibrary || deleteConfirmName !== activeLibrary.name || !activeWorkspace) return;
    try {
      await apiFetch(`/catalog/workspaces/${activeWorkspace.id}/libraries/${activeLibrary.id}`, { method: 'DELETE' });
      toast.success(t('shell.libraryDeleted'));
      await refreshSession();
    } catch (err: any) {
      toast.error(err?.message || 'Failed to delete library');
    }
    setDeleteConfirmName('');
    setDeleteLibOpen(false);
  };

  const missingPurposes = activeLibrary?.missingBindingPurposes ?? [];

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
          <div className="w-6 h-6 rounded-lg flex items-center justify-center text-[11px] font-black transition-transform duration-200 group-hover:scale-110" style={{
            background: 'linear-gradient(135deg, hsl(var(--shell-active)), hsl(224 76% 42%))',
            color: 'white',
            boxShadow: '0 2px 8px -2px hsl(var(--shell-active) / 0.5)',
          }}>R</div>
          <span className="hidden sm:inline">RustRAG</span>
        </Link>

        {/* Desktop nav */}
        <nav className="hidden md:flex items-center gap-0.5 mr-auto">
          {NAV_KEYS.map(item => (
            <button
              key={item.path}
              onClick={() => navigate(item.path)}
              className={`shell-nav-item flex items-center gap-1.5 ${isActive(item.path) ? 'active' : ''}`}
            >
              <item.icon className="h-3.5 w-3.5" />
              <span>{t(item.key)}</span>
            </button>
          ))}
        </nav>

        {/* Mobile menu toggle */}
        <button
          className="md:hidden ml-auto p-1.5 rounded-lg transition-colors"
          onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
          style={{ color: 'hsl(var(--shell-foreground))' }}
          aria-label="Toggle navigation"
        >
          {mobileMenuOpen ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
        </button>

        {/* Right side controls */}
        <div className="hidden md:flex items-center gap-1.5 ml-auto">
          {/* Library readiness warning */}
          {activeLibrary && missingPurposes.length > 0 && (
            <div className="flex items-center gap-1 text-[11px] status-warning px-2.5 py-1 rounded-full font-semibold">
              <AlertTriangle className="h-3 w-3" />
              <span>{missingPurposes.length} {missingPurposes.length > 1 ? t('admin.bindingsMissingPlural') : t('admin.bindingsMissing')}</span>
            </div>
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
            <DropdownMenuContent align="end" className="min-w-[180px]">
              {workspaces.map(ws => (
                <DropdownMenuItem key={ws.id} onClick={() => { setActiveWorkspace(ws); setActiveLibrary(null); }}>
                  {ws.name}
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setCreateWsOpen(true)}>
                <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createWorkspace')}
              </DropdownMenuItem>
              {activeWorkspace && (
                <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteWsOpen(true); }} className="text-destructive">
                  <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteWorkspace')}
                </DropdownMenuItem>
              )}
            </DropdownMenuContent>
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
            <DropdownMenuContent align="end" className="min-w-[180px]">
              {libraries.map(lib => (
                <DropdownMenuItem key={lib.id} onClick={() => setActiveLibrary(lib)}>
                  {lib.name}
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setCreateLibOpen(true)}>
                <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createLibrary')}
              </DropdownMenuItem>
              {activeLibrary && (
                <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteLibOpen(true); }} className="text-destructive">
                  <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteLibrary')}
                </DropdownMenuItem>
              )}
            </DropdownMenuContent>
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
                  {(user?.displayName ?? 'U')[0].toUpperCase()}
                </div>
                <span className="truncate max-w-[80px] font-medium">{user?.displayName ?? 'User'}</span>
                <ChevronDown className="h-3 w-3 opacity-50" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-[180px]">
              <div className="px-2 py-1.5 text-xs text-muted-foreground font-medium">
                {user?.accessLabel ?? 'Operator'}
              </div>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => { logout(); navigate('/login'); }}>
                <LogOut className="h-3.5 w-3.5 mr-1.5" /> {t('shell.logout')}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      {/* Mobile nav overlay */}
      {mobileMenuOpen && (
        <div className="md:hidden border-b p-3 space-y-1 animate-fade-in" style={{ background: 'hsl(var(--shell-bg))' }}>
          {NAV_KEYS.map(item => (
            <button
              key={item.path}
              onClick={() => { navigate(item.path); setMobileMenuOpen(false); }}
              className={`shell-nav-item flex items-center gap-2 w-full ${isActive(item.path) ? 'active' : ''}`}
            >
              <item.icon className="h-4 w-4" />
              <span>{t(item.key)}</span>
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
              <DropdownMenuContent align="start" className="min-w-[180px]">
                {workspaces.map(ws => (
                  <DropdownMenuItem key={ws.id} onClick={() => { setActiveWorkspace(ws); setActiveLibrary(null); }}>
                    {ws.name}
                  </DropdownMenuItem>
                ))}
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={() => setCreateWsOpen(true)}>
                  <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createWorkspace')}
                </DropdownMenuItem>
                {activeWorkspace && (
                  <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteWsOpen(true); }} className="text-destructive">
                    <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteWorkspace')}
                  </DropdownMenuItem>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button className="flex items-center gap-1.5 text-xs px-2.5 py-1.5 rounded-lg font-medium" style={{ color: 'hsl(var(--shell-foreground))', background: 'hsl(var(--shell-hover))', border: '1px solid hsl(var(--shell-border))' }}>
                  <span className="truncate max-w-[120px]">{activeLibrary?.name ?? t('shell.noLibrary')}</span>
                  <ChevronDown className="h-3 w-3 opacity-50" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="min-w-[180px]">
                {libraries.map(lib => (
                  <DropdownMenuItem key={lib.id} onClick={() => setActiveLibrary(lib)}>
                    {lib.name}
                  </DropdownMenuItem>
                ))}
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={() => setCreateLibOpen(true)}>
                  <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('shell.createLibrary')}
                </DropdownMenuItem>
                {activeLibrary && (
                  <DropdownMenuItem onClick={() => { setDeleteConfirmName(''); setDeleteLibOpen(true); }} className="text-destructive">
                    <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('shell.deleteLibrary')}
                  </DropdownMenuItem>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      )}

      {/* Main content */}
      <main className="flex-1 min-h-0 flex flex-col overflow-hidden">
        {children}
      </main>

      {/* Footer */}
      <footer className="h-8 flex items-center justify-center px-4 gap-4 shrink-0 text-[11px] text-muted-foreground border-t" style={{
        background: 'linear-gradient(180deg, hsl(var(--background)), hsl(var(--muted) / 0.3))',
      }}>
        <span className="font-medium">{BUILD_VERSION_LABEL}</span>
        <span className="hidden sm:inline">{t('common.copyright', { year: new Date().getFullYear() })}</span>
        <a
          href="https://github.com/mlimarenko/RustRAG"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1.5 hover:text-foreground transition-colors duration-200 font-medium"
        >
          <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="currentColor"><path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0024 12c0-6.63-5.37-12-12-12z"/></svg>
          GitHub
        </a>
      </footer>

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
              <Input id="ws-name" value={newWsName} onChange={e => setNewWsName(e.target.value)} placeholder="My Workspace" className="mt-1.5" />
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
              <Input id="lib-name" value={newLibName} onChange={e => setNewLibName(e.target.value)} placeholder="My Library" className="mt-1.5" />
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
            <DialogDescription dangerouslySetInnerHTML={{ __html: t('shell.deleteWorkspaceDesc', { name: activeWorkspace?.name }) }} />
          </DialogHeader>
          <div>
            <Label htmlFor="del-ws-confirm">{t('shell.typeToConfirm', { name: activeWorkspace?.name })}</Label>
            <Input id="del-ws-confirm" value={deleteConfirmName} onChange={e => setDeleteConfirmName(e.target.value)} className="mt-1.5" />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteWsOpen(false)}>{t('shell.cancel')}</Button>
            <Button variant="destructive" onClick={handleDeleteWorkspace} disabled={deleteConfirmName !== activeWorkspace?.name}>{t('shell.delete')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={deleteLibOpen} onOpenChange={setDeleteLibOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('shell.deleteLibraryTitle')}</DialogTitle>
            <DialogDescription dangerouslySetInnerHTML={{ __html: t('shell.deleteLibraryDesc', { name: activeLibrary?.name }) }} />
          </DialogHeader>
          <div>
            <Label htmlFor="del-lib-confirm">{t('shell.typeToConfirm', { name: activeLibrary?.name })}</Label>
            <Input id="del-lib-confirm" value={deleteConfirmName} onChange={e => setDeleteConfirmName(e.target.value)} className="mt-1.5" />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteLibOpen(false)}>{t('shell.cancel')}</Button>
            <Button variant="destructive" onClick={handleDeleteLibrary} disabled={deleteConfirmName !== activeLibrary?.name}>{t('shell.delete')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
