import { useCallback, useEffect, useMemo, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import {
  BookOpen,
  Copy,
  Eye,
  FileEdit,
  FileText,
  Key,
  Link,
  Loader2,
  MessageSquare,
  MonitorCheck,
  Plus,
  ScrollText,
  Search,
  Settings,
  ShieldCheck,
  Trash2,
  UserCog,
} from 'lucide-react';
import { adminApi } from '@/api';
import type { CatalogLibraryResponse, CatalogWorkspaceResponse } from '@/api/admin';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { mapToken } from '@/adapters/admin';
import { errorMessage } from '@/lib/errorMessage';
import { buildMintTokenRequest } from '@/pages/admin/tokenMint';
import type { APIToken } from '@/types';

// Permission metadata: human label, icon, grouping, implication graph.
// The flat `snake_case` names matched the backend wire format but
// were unreadable in the UI — operators had to guess what
// `credential_admin` meant. Icons + short labels make the hierarchy
// scannable at a glance.
type PermMeta = {
  key: string;
  label: string;
  icon: typeof ShieldCheck;
  group: 'admin' | 'workspace' | 'content' | 'operations';
  wsOnly?: boolean; // shown only in workspace-scope tokens
};

const PERM_META: PermMeta[] = [
  { key: 'iam_admin',        label: 'Full admin (all features)',   icon: ShieldCheck,   group: 'admin',      wsOnly: true },
  { key: 'workspace_admin',  label: 'Workspace admin',            icon: UserCog,       group: 'workspace',  wsOnly: true },
  { key: 'workspace_read',   label: 'Workspace read',             icon: Eye,           group: 'workspace',  wsOnly: true },
  { key: 'library_write',    label: 'Library write + import',     icon: FileEdit,      group: 'content' },
  { key: 'library_read',     label: 'Library read + export',      icon: BookOpen,      group: 'content' },
  { key: 'document_write',   label: 'Document upload / edit',     icon: FileEdit,      group: 'content' },
  { key: 'document_read',    label: 'Document read / search',     icon: FileText,      group: 'content' },
  { key: 'credential_admin', label: 'API credentials',            icon: Key,           group: 'operations', wsOnly: true },
  { key: 'connector_admin',  label: 'Connector admin',            icon: Link,          group: 'operations', wsOnly: true },
  { key: 'binding_admin',    label: 'AI model bindings',          icon: Settings,      group: 'operations' },
  { key: 'query_run',        label: 'Query / RAG assistant',      icon: MessageSquare, group: 'operations' },
  { key: 'ops_read',         label: 'Ops & dashboard',            icon: MonitorCheck,  group: 'operations', wsOnly: true },
  { key: 'audit_read',       label: 'Audit log',                  icon: ScrollText,    group: 'operations', wsOnly: true },
];

const PERM_GROUP_LABELS: Record<string, string> = {
  admin: 'Admin',
  workspace: 'Workspace',
  content: 'Library & content',
  operations: 'Operations',
};

const PERMISSION_IMPLIES: Record<string, string[]> = {
  iam_admin: [
    'workspace_admin', 'workspace_read', 'library_read', 'library_write',
    'document_read', 'document_write', 'credential_admin', 'connector_admin',
    'binding_admin', 'query_run', 'ops_read', 'audit_read',
  ],
  workspace_admin: [
    'workspace_read', 'library_read', 'library_write', 'document_read',
    'document_write', 'credential_admin', 'connector_admin', 'binding_admin',
    'query_run', 'ops_read', 'audit_read',
  ],
  library_write: ['library_read', 'document_read', 'document_write'],
  library_read: ['document_read'],
  document_write: ['document_read'],
};

function impliedPermissions(selected: string[]): Set<string> {
  const implied = new Set<string>();
  for (const perm of selected) {
    for (const child of PERMISSION_IMPLIES[perm] ?? []) {
      implied.add(child);
    }
  }
  return implied;
}

function visiblePermGroups(scope: TokenScope): { group: string; perms: PermMeta[] }[] {
  // System and workspace scopes see every permission. Library scope
  // hides workspace-only entries.
  const filtered = PERM_META.filter(
    (p) => scope === 'system' || scope === 'workspace' || !p.wsOnly,
  );
  const groups: Record<string, PermMeta[]> = {};
  for (const p of filtered) {
    (groups[p.group] ??= []).push(p);
  }
  return Object.entries(groups).map(([group, perms]) => ({ group, perms }));
}

type TokenScope = 'system' | 'workspace' | 'library';

type AccessTabProps = {
  t: TFunction;
  activeWorkspaceId: string | undefined;
  active: boolean;
};

function tokenStatusCls(status: string): string {
  if (status === 'active') return 'status-ready';
  if (status === 'expired') return 'status-warning';
  return 'status-failed';
}

function humanizeTokenStatus(status: APIToken['status'], t: TFunction): string {
  switch (status) {
    case 'active':
      return t('admin.active');
    case 'expired':
      return t('admin.expired');
    case 'revoked':
      return t('admin.revoked');
    default:
      return status;
  }
}

export function AccessTab({ t, activeWorkspaceId, active }: AccessTabProps) {
  const [tokens, setTokens] = useState<APIToken[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedToken, setSelectedToken] = useState<APIToken | null>(null);
  const [tokenSearch, setTokenSearch] = useState('');

  const [createOpen, setCreateOpen] = useState(false);
  const [createdToken, setCreatedToken] = useState<string | null>(null);
  const [showToken, setShowToken] = useState(false);

  const [tokenLabel, setTokenLabel] = useState('');
  const [tokenExpiry, setTokenExpiry] = useState('90');
  const [tokenScope, setTokenScope] = useState<TokenScope>('workspace');
  const [tokenWorkspaceId, setTokenWorkspaceId] = useState('');
  const [tokenWorkspaces, setTokenWorkspaces] = useState<CatalogWorkspaceResponse[]>([]);
  const [tokenWorkspacesLoading, setTokenWorkspacesLoading] = useState(false);
  const [tokenWorkspacesError, setTokenWorkspacesError] = useState<string | null>(null);
  const [tokenLibraries, setTokenLibraries] = useState<CatalogLibraryResponse[]>([]);
  const [tokenLibrariesLoading, setTokenLibrariesLoading] = useState(false);
  const [tokenLibrariesError, setTokenLibrariesError] = useState<string | null>(null);
  const [selectedLibraryIds, setSelectedLibraryIds] = useState<string[]>([]);
  const [selectedPermissions, setSelectedPermissions] = useState<string[]>([]);
  const [minting, setMinting] = useState(false);
  const selectedActiveLibraryIds = useMemo(
    () =>
      selectedLibraryIds.filter((libraryId) =>
        tokenLibraries.some((library) => library.id === libraryId),
      ),
    [selectedLibraryIds, tokenLibraries],
  );

  const loadTokens = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    adminApi
      .listTokens()
      .then((data) => {
        const list = Array.isArray(data) ? data : [];
        setTokens(list.map(mapToken));
      })
      .catch((err: unknown) =>
        setLoadError(errorMessage(err, t('admin.loadTokensFailed'))),
      )
      .finally(() => setLoading(false));
  }, [t]);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) {
        loadTokens();
      }
    });
    return () => {
      cancelled = true;
    };
  }, [active, loadTokens]);

  // Load available workspaces when the create-token dialog opens.
  useEffect(() => {
    if (!createOpen) return;
    let cancelled = false;
    void (async () => {
      setTokenWorkspacesLoading(true);
      setTokenWorkspacesError(null);
      try {
        const workspaceRows = await adminApi.listWorkspaces();
        if (cancelled) return;
        const nextWorkspaces = Array.isArray(workspaceRows) ? workspaceRows : [];
        setTokenWorkspaces(nextWorkspaces);
        setTokenWorkspaceId((current) => {
          if (current && nextWorkspaces.some((workspace) => workspace.id === current)) {
            return current;
          }
          if (
            activeWorkspaceId &&
            nextWorkspaces.some((workspace) => workspace.id === activeWorkspaceId)
          ) {
            return activeWorkspaceId;
          }
          return nextWorkspaces[0]?.id ?? '';
        });
      } catch (err: unknown) {
        if (cancelled) return;
        setTokenWorkspaces([]);
        setTokenWorkspaceId('');
        const message = errorMessage(err, t('admin.loadWorkspacesFailed'));
        setTokenWorkspacesError(message);
        toast.error(message);
      } finally {
        if (!cancelled) {
          setTokenWorkspacesLoading(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeWorkspaceId, createOpen, t]);

  // Load libraries that belong to the selected workspace.
  useEffect(() => {
    if (!createOpen || !tokenWorkspaceId) {
      queueMicrotask(() => {
        setTokenLibraries([]);
        setTokenLibrariesLoading(false);
        setTokenLibrariesError(null);
      });
      return;
    }
    let cancelled = false;
    void (async () => {
      setTokenLibrariesLoading(true);
      setTokenLibrariesError(null);
      try {
        const libraryRows = await adminApi.listLibraries(tokenWorkspaceId);
        if (cancelled) return;
        const nextLibraries = Array.isArray(libraryRows) ? libraryRows : [];
        setTokenLibraries(nextLibraries);
        setSelectedLibraryIds((current) =>
          current.filter((libraryId) =>
            nextLibraries.some((library) => library.id === libraryId),
          ),
        );
      } catch (err: unknown) {
        if (cancelled) return;
        setTokenLibraries([]);
        const message = errorMessage(err, t('admin.loadLibrariesFailed'));
        setTokenLibrariesError(message);
        toast.error(message);
      } finally {
        if (!cancelled) {
          setTokenLibrariesLoading(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [createOpen, tokenWorkspaceId, t]);

  const handleCreate = () => {
    if (!tokenWorkspaceId) {
      toast.error(t('admin.tokenRequiresWorkspace'));
      return;
    }
    setMinting(true);
    adminApi
      .mintToken(
        buildMintTokenRequest({
          label: tokenLabel.trim(),
          expiryDays: tokenExpiry,
          scope: tokenScope,
          workspaceId: tokenWorkspaceId,
          libraryIds: selectedActiveLibraryIds,
          permissionKinds: selectedPermissions,
        }),
      )
      .then((data) => {
        setCreatedToken(data.token ?? '');
        setCreateOpen(false);
        setShowToken(true);
        setTokenLabel('');
        setTokenExpiry('90');
        setTokenScope('workspace');
        setSelectedPermissions([]);
        setSelectedLibraryIds([]);
        setTokenWorkspaceId(activeWorkspaceId ?? '');
        loadTokens();
      })
      .catch((err: unknown) =>
        toast.error(errorMessage(err, t('admin.createTokenFailed'))),
      )
      .finally(() => setMinting(false));
  };

  const handleRevoke = (token: APIToken) => {
    adminApi
      .revokeToken(token.id)
      .then(() => {
        loadTokens();
        setSelectedToken(null);
      })
      .catch((err: unknown) =>
        toast.error(errorMessage(err, t('admin.revokeTokenFailed'))),
      );
  };

  const filteredTokens = tokens.filter(
    (token) => !tokenSearch || token.label.toLowerCase().includes(tokenSearch.toLowerCase()),
  );
  const selectedTokenWorkspaceName = tokenWorkspaces.find(
    (workspace) => workspace.id === tokenWorkspaceId,
  )?.displayName;
  const canCreate =
    Boolean(
      tokenLabel.trim() &&
        selectedPermissions.length > 0 &&
        // System scope needs no workspace; workspace/library need one.
        (tokenScope === 'system' || tokenWorkspaceId) &&
        (tokenScope === 'system' || !tokenWorkspacesLoading) &&
        (tokenScope === 'system' || !tokenWorkspacesError) &&
        (tokenScope !== 'library' ||
          (!tokenLibrariesLoading &&
            !tokenLibrariesError &&
            selectedActiveLibraryIds.length > 0)),
    ) && !minting;

  return (
    <>
      <div className="flex items-center justify-between mb-5">
        <div className="flex gap-4 text-xs font-semibold">
          {loading ? (
            <span className="text-muted-foreground flex items-center gap-1.5">
              <Loader2 className="h-3 w-3 animate-spin" /> {t('admin.loading')}
            </span>
          ) : loadError ? (
            <span className="text-status-failed">{loadError}</span>
          ) : (
            <>
              <span className="text-muted-foreground">
                {tokens.length} {t('admin.total')}
              </span>
              <span className="text-status-ready">
                {tokens.filter((token) => token.status === 'active').length} {t('admin.active')}
              </span>
            </>
          )}
        </div>
        <div className="flex gap-2">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
            <Input
              className="h-9 pl-9 w-48 text-sm"
              placeholder={t('admin.searchTokens')}
              value={tokenSearch}
              onChange={(e) => setTokenSearch(e.target.value)}
            />
          </div>
          <Button size="sm" onClick={() => setCreateOpen(true)}>
            <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.createToken')}
          </Button>
        </div>
      </div>

      <div className="flex gap-6">
        <div className="flex-1 space-y-1.5">
          {filteredTokens.map((token) => (
            <button
              key={token.id}
              className={`w-full flex items-center gap-3 p-4 rounded-xl text-left transition-all duration-200 ${
                selectedToken?.id === token.id
                  ? 'bg-card shadow-lifted border border-primary/15'
                  : 'hover:bg-accent/50 border border-transparent hover:shadow-soft'
              }`}
              onClick={() => setSelectedToken(token)}
            >
              <div className="w-9 h-9 rounded-xl bg-surface-sunken flex items-center justify-center shrink-0">
                <Key className="h-4 w-4 text-muted-foreground" />
              </div>
              <div className="flex-1 min-w-0">
                <div className="text-sm font-bold truncate">{token.label}</div>
                <div className="text-xs text-muted-foreground mt-0.5 font-medium">
                  {token.tokenPrefix}... · {token.scopeSummary}
                </div>
              </div>
              <span className={`status-badge ${tokenStatusCls(token.status)}`}>
                {humanizeTokenStatus(token.status, t)}
              </span>
            </button>
          ))}
          {!loading && !loadError && filteredTokens.length === 0 && (
            <div className="text-sm text-muted-foreground text-center p-8">
              {t('admin.noTokens')}
            </div>
          )}
        </div>

        {selectedToken && (
          <div className="w-80 shrink-0 workbench-surface p-5 space-y-4 animate-slide-in-right">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-bold">{selectedToken.label}</h3>
              <span className={`status-badge ${tokenStatusCls(selectedToken.status)}`}>
                {humanizeTokenStatus(selectedToken.status, t)}
              </span>
            </div>
            <div className="space-y-2.5 text-sm">
              {[
                [t('admin.prefix'), selectedToken.tokenPrefix + '...'],
                [t('admin.scope'), selectedToken.scopeSummary],
                [t('admin.principal'), selectedToken.principalLabel],
                [t('admin.issuedBy'), selectedToken.issuedBy],
                [
                  t('admin.expires'),
                  selectedToken.expiresAt
                    ? new Date(selectedToken.expiresAt).toLocaleDateString()
                    : t('admin.never'),
                ],
                [
                  t('admin.lastUsed'),
                  selectedToken.lastUsedAt
                    ? new Date(selectedToken.lastUsedAt).toLocaleDateString()
                    : t('admin.never'),
                ],
              ].map(([k, v]) => (
                <div key={k} className="flex justify-between">
                  <span className="text-muted-foreground">{k}</span>
                  <span className="font-mono text-xs font-bold">{v}</span>
                </div>
              ))}
            </div>
            {selectedToken.status === 'active' && (
              <Button
                variant="destructive"
                size="sm"
                className="w-full"
                onClick={() => handleRevoke(selectedToken)}
              >
                <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('admin.revokeToken')}
              </Button>
            )}
          </div>
        )}
      </div>

      <Dialog
        open={createOpen}
        onOpenChange={(open) => {
          setCreateOpen(open);
          if (!open) {
            setTokenLabel('');
            setTokenExpiry('90');
            setTokenScope('workspace');
            setSelectedPermissions([]);
            setSelectedLibraryIds([]);
            setTokenWorkspaces([]);
            setTokenWorkspacesLoading(false);
            setTokenWorkspacesError(null);
            setTokenLibraries([]);
            setTokenLibrariesLoading(false);
            setTokenLibrariesError(null);
            setTokenWorkspaceId(activeWorkspaceId ?? '');
          }
        }}
      >
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>{t('admin.createTokenTitle')}</DialogTitle>
            <DialogDescription>{t('admin.createTokenDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <Label>{t('admin.tokenLabel')}</Label>
              <Input
                value={tokenLabel}
                onChange={(e) => setTokenLabel(e.target.value)}
                placeholder={t('admin.tokenLabelPlaceholder')}
                className="mt-2"
              />
            </div>
            {tokenScope !== 'system' && (
              <div>
                <Label>{t('admin.tokenWorkspace')}</Label>
                <p
                  className={`mt-1 text-xs ${
                    tokenWorkspacesError ? 'text-status-failed' : 'text-muted-foreground'
                  }`}
                >
                  {tokenWorkspacesError ?? t('admin.tokenWorkspaceDesc')}
                </p>
                <Select
                  value={tokenWorkspaceId}
                  onValueChange={(workspaceId) => {
                    setTokenWorkspaceId(workspaceId);
                    setSelectedLibraryIds([]);
                  }}
                  disabled={tokenWorkspacesLoading || tokenWorkspaces.length === 0}
                >
                  <SelectTrigger className="mt-2">
                    <SelectValue
                      placeholder={
                        tokenWorkspacesLoading ? t('admin.loading') : t('admin.selectWorkspace')
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {tokenWorkspaces.map((workspace) => (
                      <SelectItem key={workspace.id} value={workspace.id}>
                        {workspace.displayName ?? workspace.id}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
            <div>
              <Label>{t('admin.tokenExpiry')}</Label>
              <Select value={tokenExpiry} onValueChange={setTokenExpiry}>
                <SelectTrigger className="mt-2">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="30">{t('admin.tokenExpiry30')}</SelectItem>
                  <SelectItem value="90">{t('admin.tokenExpiry90')}</SelectItem>
                  <SelectItem value="365">{t('admin.tokenExpiry365')}</SelectItem>
                  <SelectItem value="never">{t('admin.never')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div>
              <Label>{t('admin.tokenScope')}</Label>
              <Select
                value={tokenScope}
                onValueChange={(v) => {
                  const nextScope = v as TokenScope;
                  const allowed = new Set(
                    PERM_META
                      .filter(
                        (p) =>
                          nextScope === 'system' ||
                          nextScope === 'workspace' ||
                          !p.wsOnly,
                      )
                      .map((p) => p.key),
                  );
                  setTokenScope(nextScope);
                  setSelectedPermissions((current) =>
                    current.filter((permission) => allowed.has(permission)),
                  );
                  setSelectedLibraryIds([]);
                }}
              >
                <SelectTrigger className="mt-2">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="system">{t('admin.system')}</SelectItem>
                  <SelectItem value="workspace">{t('admin.workspace')}</SelectItem>
                  <SelectItem value="library">{t('admin.library')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            {tokenScope === 'library' && (
              <div>
                <div className="flex items-center justify-between gap-3">
                  <Label>{t('admin.tokenLibraries')}</Label>
                  {selectedTokenWorkspaceName ? (
                    <span className="text-xs text-muted-foreground">
                      {selectedTokenWorkspaceName}
                    </span>
                  ) : null}
                </div>
                <p className="mt-1 text-xs text-muted-foreground">
                  {t('admin.tokenLibrariesDesc')}
                </p>
                <div className="mt-2 space-y-1.5 max-h-40 overflow-y-auto p-3 border rounded-xl bg-surface-sunken">
                  {tokenLibrariesLoading ? (
                    <p className="text-sm text-muted-foreground flex items-center gap-1.5">
                      <Loader2 className="h-3.5 w-3.5 animate-spin" /> {t('admin.loading')}
                    </p>
                  ) : tokenLibrariesError ? (
                    <p className="text-sm text-status-failed">{tokenLibrariesError}</p>
                  ) : tokenLibraries.length === 0 ? (
                    <p className="text-sm text-muted-foreground">
                      {t('admin.tokenNoLibrariesAvailable')}
                    </p>
                  ) : (
                    tokenLibraries.map((library) => (
                      <div key={library.id} className="flex items-center gap-2.5">
                        <Checkbox
                          id={`token-library-${library.id}`}
                          checked={selectedLibraryIds.includes(library.id)}
                          onCheckedChange={(checked) =>
                            setSelectedLibraryIds((previous) =>
                              checked
                                ? previous.includes(library.id)
                                  ? previous
                                  : [...previous, library.id]
                                : previous.filter((libraryId) => libraryId !== library.id),
                            )
                          }
                        />
                        <Label
                          htmlFor={`token-library-${library.id}`}
                          className="text-sm font-normal"
                        >
                          {library.displayName ?? library.id}
                        </Label>
                      </div>
                    ))
                  )}
                </div>
              </div>
            )}
            <div>
              <Label>{t('admin.tokenPermissions')}</Label>
              <div className="mt-2 grid grid-cols-2 gap-2">
                {(() => {
                  const groups = visiblePermGroups(tokenScope);
                  const implied = impliedPermissions(selectedPermissions);
                  return groups.map(({ group, perms }) => (
                    <div key={group} className="border rounded-xl p-2.5 bg-surface-sunken space-y-1">
                      <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground mb-1">
                        {PERM_GROUP_LABELS[group] ?? group}
                      </div>
                      {perms.map((perm) => {
                        const isExplicit = selectedPermissions.includes(perm.key);
                        const isImplied = !isExplicit && implied.has(perm.key);
                        const Icon = perm.icon;
                        return (
                          <label
                            key={perm.key}
                            htmlFor={`perm-${perm.key}`}
                            className={`flex items-center gap-2 rounded-lg px-2 py-1 text-xs transition-colors ${
                              isImplied
                                ? 'opacity-50 cursor-default'
                                : 'cursor-pointer hover:bg-accent/40'
                            }`}
                          >
                            <Checkbox
                              id={`perm-${perm.key}`}
                              checked={isExplicit || isImplied}
                              disabled={isImplied}
                              className="h-3.5 w-3.5"
                              onCheckedChange={(checked) =>
                                setSelectedPermissions((prev) =>
                                  checked
                                    ? [...prev, perm.key]
                                    : prev.filter((x) => x !== perm.key),
                                )
                              }
                            />
                            <Icon className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                            <span className="font-medium truncate">{perm.label}</span>
                          </label>
                        );
                      })}
                    </div>
                  ));
                })()}
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
              {t('admin.cancel')}
            </Button>
            <Button onClick={handleCreate} disabled={!canCreate}>
              {minting ? t('admin.creating') : t('admin.create')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={showToken} onOpenChange={setShowToken}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('admin.tokenCreated')}</DialogTitle>
            <DialogDescription>{t('admin.tokenCreatedDesc')}</DialogDescription>
          </DialogHeader>
          <div className="flex items-center gap-2">
            <Input readOnly value={createdToken ?? ''} className="font-mono text-xs" />
            <Button
              variant="outline"
              size="icon"
              onClick={() => navigator.clipboard.writeText(createdToken ?? '')}
            >
              <Copy className="h-4 w-4" />
            </Button>
          </div>
          <DialogFooter>
            <Button
              onClick={() => {
                setShowToken(false);
                setCreatedToken(null);
              }}
            >
              {t('admin.done')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
