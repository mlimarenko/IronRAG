import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { z } from 'zod';
import {
  Ban,
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
import { adminApi, queries } from '@/shared/api';
import type {
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  IamPermissionKind,
  MintTokenResponse,
  TokenGrantSummaryResponse,
  TokenResponse,
} from '@/shared/api/generated';
import { DataState } from '@/shared/components/DataState';
import { RowActionsMenu, type RowAction } from '@/shared/components/layout/RowActionsMenu';
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState';
import { StatusBadge, type StatusTone } from '@/shared/components/StatusBadge';
import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import { Checkbox } from '@/shared/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { Input } from '@/shared/components/ui/input';
import { Label } from '@/shared/components/ui/label';
import { SelectItem } from '@/shared/components/ui/select';
import { mapToken } from '@/features/admin/model/adminAdapter';
import { errorMessage } from '@/shared/lib/errorMessage';
import { buildMintTokenRequest } from '@/features/admin/model/tokenMint';
import type { APIToken } from '@/shared/types';
import {
  fieldErrorMessage,
  FormInputField,
  FormSelectField,
  nonEmptyString,
  useTypedForm,
} from '@/shared/forms';

// Permission metadata: human label, icon, grouping, implication graph.
// The flat `snake_case` names matched the backend wire format but
// were unreadable in the UI — operators had to guess what
// `credential_admin` meant. Icons + short labels make the hierarchy
// scannable at a glance.
type PermMeta = {
  key: IamPermissionKind;
  icon: typeof ShieldCheck;
  group: 'admin' | 'workspace' | 'content' | 'operations';
  wsOnly?: boolean; // shown only in workspace-scope tokens
};

const PERM_META: PermMeta[] = [
  { key: 'iam_admin',        icon: ShieldCheck,   group: 'admin',      wsOnly: true },
  { key: 'workspace_admin',  icon: UserCog,       group: 'workspace',  wsOnly: true },
  { key: 'workspace_read',   icon: Eye,           group: 'workspace',  wsOnly: true },
  { key: 'library_write',    icon: FileEdit,      group: 'content' },
  { key: 'library_read',     icon: BookOpen,      group: 'content' },
  { key: 'document_write',   icon: FileEdit,      group: 'content' },
  { key: 'document_read',    icon: FileText,      group: 'content' },
  { key: 'credential_admin', icon: Key,           group: 'operations', wsOnly: true },
  { key: 'connector_admin',  icon: Link,          group: 'operations', wsOnly: true },
  { key: 'binding_admin',    icon: Settings,      group: 'operations' },
  { key: 'query_run',        icon: MessageSquare, group: 'operations' },
  { key: 'ops_read',         icon: MonitorCheck,  group: 'operations', wsOnly: true },
  { key: 'audit_read',       icon: ScrollText,    group: 'operations', wsOnly: true },
];

const PERMISSION_IMPLIES: Partial<Record<IamPermissionKind, IamPermissionKind[]>> = {
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

function impliedPermissions(selected: IamPermissionKind[]): Set<IamPermissionKind> {
  const implied = new Set<IamPermissionKind>();
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
type TokenMutationContext = {
  previousSelectedTokenId: string | null | undefined;
  previousTokens: TokenResponse[] | undefined;
};
type MintTokenVariables = {
  optimisticToken: TokenResponse;
  request: ReturnType<typeof buildMintTokenRequest>;
};

type AccessTabProps = {
  t: TFunction;
  activeWorkspaceId: string | undefined;
  active: boolean;
};

function tokenStatusTone(status: string): StatusTone {
  if (status === 'active') return 'ready';
  if (status === 'expired') return 'warning';
  return 'failed';
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

function permissionMeta(permission: string): PermMeta | undefined {
  return PERM_META.find((perm) => perm.key === permission);
}

function permissionLabel(permission: string, t: TFunction): string {
  if (!permissionMeta(permission)) {
    return permission.split('_').join(' ');
  }
  return t(`admin.permissionLabels.${permission}`);
}

function permissionGroupLabel(group: string, t: TFunction): string {
  if (group !== 'admin' && group !== 'workspace' && group !== 'content' && group !== 'operations') {
    return group;
  }
  return t(`admin.permissionGroups.${group}`);
}

function tokenScopeHeading(token: APIToken, t: TFunction): string {
  switch (token.scope.kind) {
    case 'workspace':
      return t('admin.workspace');
    case 'library':
      return t('admin.library');
    default:
      return t('admin.system');
  }
}

function tokenScopeSummary(token: APIToken, t: TFunction): string {
  if (token.scope.kind === 'system') {
    return t('admin.system');
  }
  if (token.scope.kind === 'workspace') {
    return token.scope.workspace?.displayName ?? t('admin.workspace');
  }
  const libraryNames = token.scope.libraries.map((library) => library.displayName);
  if (libraryNames.length === 0) {
    return token.scope.workspace?.displayName ?? t('admin.library');
  }
  if (libraryNames.length <= 2) {
    return libraryNames.join(', ');
  }
  const workspaceName = token.scope.workspace?.displayName;
  return workspaceName
    ? `${workspaceName} · ${libraryNames.length} ${t('admin.tokenLibraries').toLowerCase()}`
    : `${libraryNames.length} ${t('admin.tokenLibraries').toLowerCase()}`;
}

function tokenScopeLine(token: APIToken, t: TFunction): string {
  if (token.scope.kind === 'system') {
    return t('admin.system');
  }
  return `${tokenScopeHeading(token, t)}: ${tokenScopeSummary(token, t)}`;
}

function groupTokenPermissions(token: APIToken): { group: string; permissions: string[] }[] {
  const grouped = token.grants.reduce<Record<string, Set<string>>>((acc, grant) => {
    const group = permissionMeta(grant.permission)?.group ?? 'operations';
    (acc[group] ??= new Set<string>()).add(grant.permission);
    return acc;
  }, {});
  return Object.entries(grouped)
    .map(([group, permissions]) => ({
      group,
      permissions: Array.from(permissions).sort((left, right) => left.localeCompare(right)),
    }))
    .sort((left, right) => left.group.localeCompare(right.group));
}

function uniquePermissionLabels(token: APIToken, t: TFunction): string[] {
  return Array.from(new Set(token.grants.map((grant) => permissionLabel(grant.permission, t))));
}

function tokenLastUsedLabel(token: APIToken, t: TFunction): string {
  return token.lastUsedAt ? new Date(token.lastUsedAt).toLocaleDateString() : t('admin.never');
}

function buildOptimisticMintedToken({
  label,
  libraryIds,
  permissionKinds,
  request,
  scope,
  tokenLibraries,
  tokenWorkspaces,
  workspaceId,
}: {
  label: string;
  libraryIds: string[];
  permissionKinds: IamPermissionKind[];
  request: ReturnType<typeof buildMintTokenRequest>;
  scope: TokenScope;
  tokenLibraries: CatalogLibraryResponse[];
  tokenWorkspaces: CatalogWorkspaceResponse[];
  workspaceId: string;
}): TokenResponse {
  const workspace = tokenWorkspaces.find((entry) => entry.id === workspaceId);
  const scopeWorkspace = scope === 'system'
    ? undefined
    : {
        id: workspaceId,
        displayName: workspace?.displayName ?? workspaceId,
      };
  const scopeLibraries = scope === 'library'
    ? tokenLibraries
        .filter((library) => libraryIds.includes(library.id))
        .map((library) => ({
          id: library.id,
          workspaceId,
          displayName: library.displayName ?? library.id,
        }))
    : [];
  const grants = permissionKinds.flatMap<TokenGrantSummaryResponse>((permissionKind) => {
    if (scope === 'system') {
      return [{
        permissionKind,
        resourceId: 'system',
        resourceKind: 'system' as const,
      }];
    }
    if (scope === 'workspace') {
      return [{
        permissionKind,
        resourceId: workspaceId,
        resourceKind: 'workspace' as const,
        ...(scopeWorkspace ? { workspace: scopeWorkspace } : {}),
      }];
    }
    return scopeLibraries.map((library) => ({
      library,
      permissionKind,
      resourceId: library.id,
      resourceKind: 'library' as const,
      ...(scopeWorkspace ? { workspace: scopeWorkspace } : {}),
    }));
  });

  return {
    expiresAt: request.expiresAt ?? null,
    grants,
    label,
    lastUsedAt: null,
    principalId: `optimistic-token-${Date.now()}`,
    revokedAt: null,
    scope: {
      kind: scope,
      libraries: scopeLibraries,
      ...(scopeWorkspace ? { workspace: scopeWorkspace } : {}),
    },
    status: 'active',
    tokenPrefix: 'creating',
  };
}

export function AccessTab({ t, activeWorkspaceId, active }: AccessTabProps) {
  const queryClient = useQueryClient();
  const tokenListQuery = queries.listIamTokensOptions();

  const [selectedTokenId, setSelectedTokenId] = useState<string | null | undefined>(undefined);
  const [tokenSearch, setTokenSearch] = useState('');
  const [deleteToken, setDeleteToken] = useState<APIToken | null>(null);

  const [createOpen, setCreateOpen] = useState(false);
  const [createdToken, setCreatedToken] = useState<string | null>(null);
  const [showToken, setShowToken] = useState(false);

  const tokenFormSchema = useMemo(
    () =>
      z.object({
        expiryDays: z.enum(['30', '90', '365', 'never']),
        label: nonEmptyString(t('admin.tokenLabel')),
        libraryIds: z.array(z.string()),
        permissionKinds: z
          .array(z.custom<IamPermissionKind>(
            value => typeof value === 'string' && PERM_META.some(perm => perm.key === value),
            { message: t('admin.tokenPermissions') },
          ))
          .min(1, t('admin.tokenPermissions')),
        scope: z.enum(['system', 'workspace', 'library']),
        workspaceId: z.string(),
      }).superRefine((values, context) => {
        if (values.scope !== 'system' && !values.workspaceId) {
          context.addIssue({
            code: 'custom',
            message: t('admin.tokenRequiresWorkspace'),
            path: ['workspaceId'],
          });
        }
        if (values.scope === 'library' && values.libraryIds.length === 0) {
          context.addIssue({
            code: 'custom',
            message: t('admin.tokenLibraries'),
            path: ['libraryIds'],
          });
        }
      }),
    [t],
  );
  const tokenForm = useTypedForm({
    schema: tokenFormSchema,
    defaultValues: {
      expiryDays: '90',
      label: '',
      libraryIds: [],
      permissionKinds: [],
      scope: 'workspace',
      workspaceId: activeWorkspaceId ?? '',
    },
    mode: 'onChange',
  });
  const tokenScope = tokenForm.watch('scope');
  const tokenWorkspaceId = tokenForm.watch('workspaceId');
  const selectedLibraryIds = tokenForm.watch('libraryIds');
  const selectedPermissions = tokenForm.watch('permissionKinds');
  const {
    getValues: getTokenValues,
    reset: resetTokenForm,
    setValue: setTokenValue,
  } = tokenForm;
  const tokensQuery = useQuery({
    ...tokenListQuery,
    enabled: active,
  });
  const tokens = useMemo<APIToken[]>(() => {
    const list = Array.isArray(tokensQuery.data) ? tokensQuery.data : [];
    return list.map(mapToken);
  }, [tokensQuery.data]);
  const selectedToken = useMemo<APIToken | null>(() => {
    if (selectedTokenId === null) return null;
    if (tokens.length === 0) return null;
    return tokens.find((token) => token.id === selectedTokenId) ?? tokens[0] ?? null;
  }, [selectedTokenId, tokens]);
  const loading = tokensQuery.isLoading && active;
  const loadError = tokensQuery.error
    ? errorMessage(tokensQuery.error, t('admin.loadTokensFailed'))
    : null;

  const workspacesQuery = useQuery({
    ...queries.listCatalogWorkspacesOptions(),
    enabled: createOpen,
  });
  const tokenWorkspaces = useMemo<CatalogWorkspaceResponse[]>(
    () => (Array.isArray(workspacesQuery.data) ? workspacesQuery.data : []),
    [workspacesQuery.data],
  );
  const tokenWorkspacesLoading = workspacesQuery.isLoading && createOpen;
  const tokenWorkspacesError = workspacesQuery.error
    ? errorMessage(workspacesQuery.error, t('admin.loadWorkspacesFailed'))
    : null;
  const defaultTokenWorkspaceId = useMemo(() => {
    if (tokenWorkspaces.length === 0) return '';
    if (
      activeWorkspaceId &&
      tokenWorkspaces.some((workspace) => workspace.id === activeWorkspaceId)
    ) {
      return activeWorkspaceId;
    }
    return tokenWorkspaces[0]?.id ?? '';
  }, [activeWorkspaceId, tokenWorkspaces]);
  const selectedTokenWorkspaceId =
    tokenWorkspaces.some((workspace) => workspace.id === tokenWorkspaceId)
      ? tokenWorkspaceId
      : defaultTokenWorkspaceId;

  useEffect(() => {
    if (!createOpen || tokenScope === 'system' || !defaultTokenWorkspaceId) {
      return;
    }
    if (tokenWorkspaces.some((workspace) => workspace.id === tokenWorkspaceId)) {
      return;
    }
    setTokenValue('workspaceId', defaultTokenWorkspaceId, {
      shouldDirty: false,
      shouldValidate: true,
    });
  }, [
    createOpen,
    defaultTokenWorkspaceId,
    setTokenValue,
    tokenScope,
    tokenWorkspaceId,
    tokenWorkspaces,
  ]);

  useEffect(() => {
    if (workspacesQuery.error) {
      toast.error(errorMessage(workspacesQuery.error, t('admin.loadWorkspacesFailed')));
    }
  }, [workspacesQuery.error, t]);

  const librariesQuery = useQuery({
    ...queries.listCatalogLibrariesOptions({ path: { workspaceId: selectedTokenWorkspaceId } }),
    enabled: createOpen && Boolean(selectedTokenWorkspaceId),
  });
  const tokenLibraries = useMemo<CatalogLibraryResponse[]>(
    () => (Array.isArray(librariesQuery.data) ? librariesQuery.data : []),
    [librariesQuery.data],
  );
  const tokenLibrariesLoading = librariesQuery.isLoading && createOpen && Boolean(selectedTokenWorkspaceId);
  const tokenLibrariesError = librariesQuery.error
    ? errorMessage(librariesQuery.error, t('admin.loadLibrariesFailed'))
    : null;

  useEffect(() => {
    if (librariesQuery.error) {
      toast.error(errorMessage(librariesQuery.error, t('admin.loadLibrariesFailed')));
    }
  }, [librariesQuery.error, t]);

  const selectedActiveLibraryIds = useMemo(
    () =>
      selectedLibraryIds.filter((libraryId) =>
        tokenLibraries.some((library) => library.id === libraryId),
      ),
    [selectedLibraryIds, tokenLibraries],
  );

  const mintTokenMutation = useMutation<
    MintTokenResponse,
    unknown,
    MintTokenVariables,
    TokenMutationContext
  >({
    mutationKey: ['admin', 'iam', 'tokens', 'mint'],
    mutationFn: ({ request }) => adminApi.mintToken(request),
    onMutate: async ({ optimisticToken }) => {
      await queryClient.cancelQueries({ queryKey: tokenListQuery.queryKey });
      const previousTokens = queryClient.getQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
      );
      const previousSelectedTokenId = selectedTokenId;
      queryClient.setQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
        (current = []) => [optimisticToken, ...current],
      );
      setSelectedTokenId(optimisticToken.principalId);
      setCreateOpen(false);
      return { previousSelectedTokenId, previousTokens };
    },
    onSuccess: (data, { optimisticToken }) => {
      queryClient.setQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
        (current = []) =>
          current.map((token) =>
            token.principalId === optimisticToken.principalId
              ? data.apiToken
              : token,
          ),
      );
      setSelectedTokenId(data.apiToken.principalId);
      setCreatedToken(data.token ?? '');
      setShowToken(true);
      resetTokenForm({
        expiryDays: '90',
        label: '',
        libraryIds: [],
        permissionKinds: [],
        scope: 'workspace',
        workspaceId: activeWorkspaceId ?? '',
      });
    },
    onError: (_err, _variables, context) => {
      if (context) {
        queryClient.setQueryData(tokenListQuery.queryKey, context.previousTokens);
        setSelectedTokenId(context.previousSelectedTokenId);
      }
      setCreateOpen(true);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: tokenListQuery.queryKey });
    },
  });

  const revokeTokenMutation = useMutation<
    void,
    unknown,
    APIToken,
    TokenMutationContext
  >({
    mutationKey: ['admin', 'iam', 'tokens', 'revoke'],
    mutationFn: (token) => adminApi.revokeToken(token.id),
    onMutate: async (token) => {
      await queryClient.cancelQueries({ queryKey: tokenListQuery.queryKey });
      const previousTokens = queryClient.getQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
      );
      const previousSelectedTokenId = selectedTokenId;
      queryClient.setQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
        (current = []) =>
          current.map((candidate) =>
            candidate.principalId === token.id
              ? {
                  ...candidate,
                  revokedAt: new Date().toISOString(),
                  status: 'revoked',
                }
              : candidate,
          ),
      );
      setSelectedTokenId(token.id);
      return { previousSelectedTokenId, previousTokens };
    },
    onError: (err, _token, context) => {
      if (context) {
        queryClient.setQueryData(tokenListQuery.queryKey, context.previousTokens);
        setSelectedTokenId(context.previousSelectedTokenId);
      }
      toast.error(
        t('admin.mutations.tokenRevoke.failed', {
          error: errorMessage(err, t('admin.revokeTokenFailed')),
        }),
      );
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: tokenListQuery.queryKey });
    },
  });

  const deleteTokenMutation = useMutation<
    void,
    unknown,
    APIToken,
    TokenMutationContext
  >({
    mutationKey: ['admin', 'iam', 'tokens', 'delete'],
    mutationFn: (token) => adminApi.deleteToken(token.id),
    onMutate: async (token) => {
      await queryClient.cancelQueries({ queryKey: tokenListQuery.queryKey });
      const previousTokens = queryClient.getQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
      );
      const previousSelectedTokenId = selectedTokenId;
      queryClient.setQueryData<TokenResponse[]>(
        tokenListQuery.queryKey,
        (current = []) =>
          current.filter((candidate) => candidate.principalId !== token.id),
      );
      setSelectedTokenId(
        previousSelectedTokenId === token.id
          ? (previousTokens?.find((candidate) => candidate.principalId !== token.id)
              ?.principalId ?? null)
          : previousSelectedTokenId,
      );
      setDeleteToken(null);
      return { previousSelectedTokenId, previousTokens };
    },
    onError: (err, _token, context) => {
      if (context) {
        queryClient.setQueryData(tokenListQuery.queryKey, context.previousTokens);
        setSelectedTokenId(context.previousSelectedTokenId);
      }
      toast.error(
        t('admin.mutations.tokenDelete.failed', {
          error: errorMessage(err, t('admin.deleteTokenFailed')),
        }),
      );
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: tokenListQuery.queryKey });
    },
  });

  const handleCreate = tokenForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        const workspaceId = values.scope === 'system' ? '' : values.workspaceId;
        const libraryIds = values.scope === 'library' ? selectedActiveLibraryIds : [];
        const request = buildMintTokenRequest({
          label: values.label,
          expiryDays: values.expiryDays,
          scope: values.scope,
          workspaceId,
          libraryIds,
          permissionKinds: values.permissionKinds,
        });
        return mintTokenMutation.mutateAsync({
          optimisticToken: buildOptimisticMintedToken({
            label: values.label.trim(),
            libraryIds,
            permissionKinds: values.permissionKinds,
            request,
            scope: values.scope,
            tokenLibraries,
            tokenWorkspaces,
            workspaceId,
          }),
          request,
        });
      },
    },
    {
      errorMessage: err =>
        t('admin.mutations.tokenMint.failed', {
          error: errorMessage(err, t('admin.createTokenFailed')),
        }),
    },
  );

  const handleRevoke = (token: APIToken) => {
    revokeTokenMutation.mutate(token);
  };

  const handleDeleteToken = () => {
    if (!deleteToken || deleteToken.status !== 'revoked') {
      setDeleteToken(null);
      return;
    }
    deleteTokenMutation.mutate(deleteToken);
  };

  const tokenRowActions = (token: APIToken): RowAction[] => {
    const actions: RowAction[] = [];
    if (token.status === 'active') {
      actions.push({
        key: 'revoke',
        label: t('admin.revokeToken'),
        icon: <Ban className="h-3.5 w-3.5" />,
        onSelect: () => handleRevoke(token),
        disabled: revokeTokenMutation.isPending,
      });
    }
    if (token.status === 'revoked') {
      actions.push({
        key: 'delete',
        label: t('admin.deleteToken'),
        icon: <Trash2 className="h-3.5 w-3.5" />,
        onSelect: () => setDeleteToken(token),
        destructive: true,
        disabled: deleteTokenMutation.isPending,
      });
    }
    return actions;
  };

  const filteredTokens = tokens.filter(
    (token) => !tokenSearch || token.label.toLowerCase().includes(tokenSearch.toLowerCase()),
  );
  const selectedTokenWorkspaceName = tokenWorkspaces.find(
    (workspace) => workspace.id === selectedTokenWorkspaceId,
  )?.displayName;
  const canCreate =
    Boolean(
      tokenForm.formState.isValid &&
        // System scope needs no workspace; workspace/library need one.
        (tokenScope === 'system' || selectedTokenWorkspaceId) &&
        (tokenScope === 'system' || !tokenWorkspacesLoading) &&
        (tokenScope === 'system' || !tokenWorkspacesError) &&
        (tokenScope !== 'library' ||
          (!tokenLibrariesLoading &&
            !tokenLibrariesError &&
            selectedActiveLibraryIds.length > 0)),
    ) && !mintTokenMutation.isPending;
  const selectedTokenPermissionGroups = selectedToken ? groupTokenPermissions(selectedToken) : [];

  return (
    <>
      <div className="flex flex-col">
        <div className="mb-5 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex gap-4 text-xs font-semibold">
            {loading ? (
              <span className="text-muted-foreground flex items-center gap-1.5">
                <Loader2 className="h-3.5 w-3.5 animate-spin" /> {t('admin.loading')}
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
            <Button
              size="sm"
              onClick={() => {
                resetTokenForm({
                  expiryDays: '90',
                  label: '',
                  libraryIds: [],
                  permissionKinds: [],
                  scope: 'workspace',
                  workspaceId: activeWorkspaceId ?? '',
                });
                setCreateOpen(true);
              }}
            >
              <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.createToken')}
            </Button>
          </div>
        </div>

        <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_380px] xl:items-start">
          <div className="min-w-0">
            <div className="space-y-3 p-3 xl:hidden">
              {filteredTokens.map((token) => {
                const selected = selectedToken?.id === token.id;
                const actions = tokenRowActions(token);
                return (
                  <article
                    key={token.id}
                    aria-selected={selected}
                    className={`workbench-surface p-4 transition-all ${
                      selected ? 'border-primary/40 bg-primary/5' : ''
                    }`}
                  >
                    <button
                      type="button"
                      className="w-full min-w-0 text-left"
                      onClick={() => setSelectedTokenId(token.id)}
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="truncate text-sm font-bold">{token.label}</div>
                          <div className="mt-1 font-mono text-xs text-muted-foreground">
                            {token.tokenPrefix}...
                          </div>
                        </div>
                        <StatusBadge tone={tokenStatusTone(token.status)} className="shrink-0">
                          {humanizeTokenStatus(token.status, t)}
                        </StatusBadge>
                      </div>
                      <div className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
                        <span className="text-muted-foreground">{t('admin.scope')}</span>
                        <span className="truncate font-semibold">{tokenScopeLine(token, t)}</span>
                        <span className="text-muted-foreground">{t('admin.lastUsed')}</span>
                        <span className="tabular-nums">{tokenLastUsedLabel(token, t)}</span>
                      </div>
                      <div className="mt-3 flex flex-wrap gap-1.5">
                        {uniquePermissionLabels(token, t).slice(0, 3).map((label) => (
                          <Badge key={label} variant="outline" className="max-w-full truncate">
                            {label}
                          </Badge>
                        ))}
                        {uniquePermissionLabels(token, t).length > 3 ? (
                          <Badge variant="outline">+{uniquePermissionLabels(token, t).length - 3}</Badge>
                        ) : null}
                      </div>
                    </button>
                    {actions.length > 0 && (
                      <div className="mt-4 flex justify-end">
                        <RowActionsMenu
                          actions={actions}
                          className="w-full sm:w-8"
                          label={t('documents.actions')}
                        />
                      </div>
                    )}
                  </article>
                );
              })}
            </div>
            <table className="hidden w-full min-w-[880px] table-fixed text-sm xl:table">
              <colgroup>
                <col className="w-[22%]" />
                <col className="w-[16%]" />
                <col className="w-[30%]" />
                <col className="w-[12%]" />
                <col className="w-[10%]" />
                <col className="w-[10%]" />
              </colgroup>
              <thead className="sticky top-0 z-10 bg-card">
                <tr className="border-b text-left">
                  <th className="px-4 py-3 section-label">{t('admin.tokenLabel')}</th>
                  <th className="px-4 py-3 section-label">{t('admin.scope')}</th>
                  <th className="px-4 py-3 section-label">{t('admin.tokenPermissions')}</th>
                  <th className="px-4 py-3 section-label">{t('admin.lastUsed')}</th>
                  <th className="px-4 py-3 section-label">{t('admin.status')}</th>
                  <th className="px-4 py-3 section-label text-right">{t('documents.actions')}</th>
                </tr>
              </thead>
              <tbody>
                {filteredTokens.map((token) => {
                  const selected = selectedToken?.id === token.id;
                  const actions = tokenRowActions(token);
                  return (
                    <tr
                      key={token.id}
                      aria-selected={selected}
                      className={`cursor-pointer border-b border-border/50 transition-colors ${
                        selected
                          ? 'border-l-2 border-l-primary bg-primary/5'
                          : 'hover:bg-accent/30'
                      }`}
                      onClick={() => setSelectedTokenId(token.id)}
                    >
                      <td className="px-4 py-3">
                        <div className="max-w-md truncate font-semibold">{token.label}</div>
                        <div className="mt-1 font-mono text-2xs text-muted-foreground">
                          {token.tokenPrefix}...
                        </div>
                      </td>
                      <td className="px-4 py-3 text-xs">
                        <div className="font-semibold">{tokenScopeHeading(token, t)}</div>
                        {token.scope.kind !== 'system' && (
                          <div className="mt-1 truncate text-muted-foreground">
                            {tokenScopeSummary(token, t)}
                          </div>
                        )}
                      </td>
                      <td className="px-4 py-3">
                        <div className="flex flex-wrap gap-1.5">
                          {uniquePermissionLabels(token, t).slice(0, 3).map((label) => (
                            <Badge key={label} variant="outline" className="max-w-full truncate">
                              {label}
                            </Badge>
                          ))}
                          {uniquePermissionLabels(token, t).length > 3 ? (
                            <Badge variant="outline">
                              +{uniquePermissionLabels(token, t).length - 3}
                            </Badge>
                          ) : null}
                        </div>
                      </td>
                      <td className="px-4 py-3 text-xs tabular-nums text-muted-foreground">
                        {tokenLastUsedLabel(token, t)}
                      </td>
                      <td className="px-4 py-3">
                        <StatusBadge tone={tokenStatusTone(token.status)}>
                          {humanizeTokenStatus(token.status, t)}
                        </StatusBadge>
                      </td>
                      <td className="px-4 py-3 text-right">
                        {actions.length > 0 && (
                          <RowActionsMenu actions={actions} label={t('documents.actions')} />
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {!loading && !loadError && filteredTokens.length === 0 && (
              <WorkbenchEmptyState title={t('admin.noTokens')} />
            )}
          </div>

        {selectedToken && (
          <aside className="w-full min-w-0 animate-slide-in-right xl:sticky xl:top-0 xl:self-start">
            <div className="workbench-surface space-y-4 p-5">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <h3 className="break-words text-sm font-bold">{selectedToken.label}</h3>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {tokenScopeLine(selectedToken, t)}
                  </p>
                </div>
                <StatusBadge tone={tokenStatusTone(selectedToken.status)} className="shrink-0">
                  {humanizeTokenStatus(selectedToken.status, t)}
                </StatusBadge>
              </div>

              {selectedToken.status === 'active' && (
                <Button
                  variant="destructive"
                  size="sm"
                  className="w-full"
                  disabled={revokeTokenMutation.isPending}
                  onClick={() => handleRevoke(selectedToken)}
                >
                  <Ban className="h-3.5 w-3.5 mr-1.5" /> {t('admin.revokeToken')}
                </Button>
              )}

              {selectedToken.status === 'revoked' && (
                <Button
                  variant="destructive"
                  size="sm"
                  className="w-full"
                  disabled={deleteTokenMutation.isPending}
                  onClick={() => setDeleteToken(selectedToken)}
                >
                  <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('admin.deleteToken')}
                </Button>
              )}

              <div className="rounded-xl bg-surface-sunken/70 p-4">
                <div className="space-y-4">
                  <div>
                    <div className="section-label">
                      {t('admin.prefix')}
                    </div>
                    <div className="mt-1 break-all font-mono text-xs font-bold">
                      {selectedToken.tokenPrefix}...
                    </div>
                  </div>
                  <div>
                    <div className="section-label">
                      {t('admin.issuedBy')}
                    </div>
                    <div className="mt-1 break-words text-sm font-medium">
                      {selectedToken.issuedBy?.displayLabel ?? t('admin.system')}
                    </div>
                  </div>
                  <div className="grid gap-3 sm:grid-cols-2">
                    <div>
                      <div className="section-label">
                        {t('admin.expires')}
                      </div>
                      <div className="mt-1 text-sm font-medium">
                        {selectedToken.expiresAt
                          ? new Date(selectedToken.expiresAt).toLocaleDateString()
                          : t('admin.never')}
                      </div>
                    </div>
                    <div>
                      <div className="section-label">
                        {t('admin.lastUsed')}
                      </div>
                      <div className="mt-1 text-sm font-medium">
                        {selectedToken.lastUsedAt
                          ? new Date(selectedToken.lastUsedAt).toLocaleDateString()
                          : t('admin.never')}
                      </div>
                    </div>
                  </div>
                </div>
              </div>

              <div className="rounded-xl bg-surface-sunken/70 p-4">
                <div className="section-label">
                  {t('admin.scope')}
                </div>
                <div className="mt-1 text-sm font-semibold">
                  {tokenScopeHeading(selectedToken, t)}
                </div>
                {selectedToken.scope.workspace ? (
                  <div className="mt-3">
                    <div className="section-label">
                      {t('admin.tokenWorkspace')}
                    </div>
                    <div className="mt-1 break-words text-sm font-medium">
                      {selectedToken.scope.workspace.displayName}
                    </div>
                  </div>
                ) : null}
                {selectedToken.scope.libraries.length > 0 ? (
                  <div className="mt-3">
                    <div className="section-label">
                      {t('admin.tokenLibraries')}
                    </div>
                    <div className="mt-2 flex flex-wrap gap-2">
                      {selectedToken.scope.libraries.map((library) => (
                        <Badge key={library.id} variant="outline" className="max-w-full break-all">
                          {library.displayName}
                        </Badge>
                      ))}
                    </div>
                  </div>
                ) : null}
              </div>

              <div className="rounded-xl bg-surface-sunken/70 p-4">
                <div className="section-label">
                  {t('admin.tokenPermissions')}
                </div>
                {selectedTokenPermissionGroups.length === 0 ? (
                  <p className="mt-2 text-sm text-muted-foreground">{t('admin.tokenNoPermissions')}</p>
                ) : (
                  <div className="mt-3 space-y-3">
                    {selectedTokenPermissionGroups.map(({ group, permissions }) => (
                      <div key={group}>
                        <div className="section-label">
                          {permissionGroupLabel(group, t)}
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          {permissions.map((permission) => (
                            <Badge key={permission} variant="outline" className="max-w-full break-all">
                              {permissionLabel(permission, t)}
                            </Badge>
                          ))}
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>

            </div>
          </aside>
        )}
        </div>
      </div>

      <Dialog open={Boolean(deleteToken)} onOpenChange={(open) => {
        if (!open) {
          setDeleteToken(null);
        }
      }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('admin.deleteTokenTitle')}</DialogTitle>
            <DialogDescription>
              {deleteToken
                ? t('admin.deleteTokenDesc', { label: deleteToken.label })
                : t('admin.deleteTokenDesc', { label: '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setDeleteToken(null)}
              disabled={deleteTokenMutation.isPending}
            >
              {t('admin.cancel')}
            </Button>
            <Button
              variant="destructive"
              onClick={handleDeleteToken}
              disabled={deleteTokenMutation.isPending}
            >
              {deleteTokenMutation.isPending ? (
                <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
              ) : (
                <Trash2 className="h-3.5 w-3.5 mr-1.5" />
              )}
              {t('admin.deleteToken')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={createOpen}
        onOpenChange={(open) => {
          setCreateOpen(open);
          if (!open) {
            resetTokenForm({
              expiryDays: '90',
              label: '',
              libraryIds: [],
              permissionKinds: [],
              scope: 'workspace',
              workspaceId: activeWorkspaceId ?? '',
            });
          }
        }}
      >
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>{t('admin.createTokenTitle')}</DialogTitle>
            <DialogDescription>{t('admin.createTokenDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <FormInputField
              formState={tokenForm.formState}
              id="admin-token-label"
              label={t('admin.tokenLabel')}
              name="label"
              registration={tokenForm.register('label')}
              placeholder={t('admin.tokenLabelPlaceholder')}
            />
            {tokenScope !== 'system' && (
              <FormSelectField
                control={tokenForm.control}
                description={tokenWorkspacesError ?? t('admin.tokenWorkspaceDesc')}
                disabled={tokenWorkspacesLoading || tokenWorkspaces.length === 0}
                formState={tokenForm.formState}
                id="admin-token-workspace"
                label={t('admin.tokenWorkspace')}
                name="workspaceId"
                onValueChange={() => {
                  setTokenValue('libraryIds', [], {
                    shouldDirty: true,
                    shouldValidate: true,
                  });
                }}
                placeholder={tokenWorkspacesLoading ? t('admin.loading') : t('admin.selectWorkspace')}
              >
                {tokenWorkspaces.map((workspace) => (
                  <SelectItem key={workspace.id} value={workspace.id}>
                    {workspace.displayName ?? workspace.id}
                  </SelectItem>
                ))}
              </FormSelectField>
            )}
            <FormSelectField
              control={tokenForm.control}
              formState={tokenForm.formState}
              id="admin-token-expiry"
              label={t('admin.tokenExpiry')}
              name="expiryDays"
            >
              <SelectItem value="30">{t('admin.tokenExpiry30')}</SelectItem>
              <SelectItem value="90">{t('admin.tokenExpiry90')}</SelectItem>
              <SelectItem value="365">{t('admin.tokenExpiry365')}</SelectItem>
              <SelectItem value="never">{t('admin.never')}</SelectItem>
            </FormSelectField>
            <FormSelectField
              control={tokenForm.control}
              formState={tokenForm.formState}
              id="admin-token-scope"
              label={t('admin.tokenScope')}
              name="scope"
              onValueChange={(value) => {
                const nextScope = value as TokenScope;
                const allowed = new Set(
                  PERM_META
                    .filter(
                      (permission) =>
                        nextScope === 'system'
                        || nextScope === 'workspace'
                        || !permission.wsOnly,
                    )
                    .map((permission) => permission.key),
                );
                setTokenValue(
                  'permissionKinds',
                  getTokenValues('permissionKinds').filter((permission) => allowed.has(permission)),
                  { shouldDirty: true, shouldValidate: true },
                );
                setTokenValue('libraryIds', [], {
                  shouldDirty: true,
                  shouldValidate: true,
                });
              }}
            >
              <SelectItem value="system">{t('admin.system')}</SelectItem>
              <SelectItem value="workspace">{t('admin.workspace')}</SelectItem>
              <SelectItem value="library">{t('admin.library')}</SelectItem>
            </FormSelectField>
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
                <div className="mt-2 space-y-1.5 max-h-40 overflow-y-auto p-3 rounded-xl bg-surface-sunken">
                  <DataState
                    query={{ isLoading: tokenLibrariesLoading, error: tokenLibrariesError, data: tokenLibraries }}
                    emptyRender={
                      <p className="text-sm text-muted-foreground">
                        {t('admin.tokenNoLibrariesAvailable')}
                      </p>
                    }
                  >
                    {(libraries) =>
                      libraries.map((library) => (
                        <div key={library.id} className="flex items-center gap-2.5">
                          <Checkbox
                            id={`token-library-${library.id}`}
                            checked={selectedLibraryIds.includes(library.id)}
                            onCheckedChange={(checked) => {
                              const nextLibraryIds = checked
                                ? selectedLibraryIds.includes(library.id)
                                  ? selectedLibraryIds
                                  : [...selectedLibraryIds, library.id]
                                : selectedLibraryIds.filter((libraryId) => libraryId !== library.id);
                              setTokenValue('libraryIds', nextLibraryIds, {
                                shouldDirty: true,
                                shouldValidate: true,
                              });
                            }}
                          />
                          <Label
                            htmlFor={`token-library-${library.id}`}
                            className="text-sm font-normal"
                          >
                            {library.displayName ?? library.id}
                          </Label>
                        </div>
                      ))
                    }
                  </DataState>
                </div>
                {fieldErrorMessage(tokenForm.formState.errors, 'libraryIds') && (
                  <p role="alert" className="mt-2 text-xs text-destructive">
                    {fieldErrorMessage(tokenForm.formState.errors, 'libraryIds')}
                  </p>
                )}
              </div>
            )}
            <div>
              <Label>{t('admin.tokenPermissions')}</Label>
              <div className="mt-2 grid grid-cols-2 gap-2">
                {(() => {
                  const groups = visiblePermGroups(tokenScope);
                  const implied = impliedPermissions(selectedPermissions);
                  return groups.map(({ group, perms }) => (
                    <div key={group} className="rounded-xl p-2.5 bg-surface-sunken space-y-1">
                      <div className="section-label mb-1">
                        {permissionGroupLabel(group, t)}
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
                              onCheckedChange={(checked) => {
                                const nextPermissions = checked
                                  ? selectedPermissions.includes(perm.key)
                                    ? selectedPermissions
                                    : [...selectedPermissions, perm.key]
                                  : selectedPermissions.filter((permission) => permission !== perm.key);
                                setTokenValue('permissionKinds', nextPermissions, {
                                  shouldDirty: true,
                                  shouldValidate: true,
                                });
                              }}
                            />
                            <Icon className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                            <span className="font-medium truncate">{permissionLabel(perm.key, t)}</span>
                          </label>
                        );
                      })}
                    </div>
                  ));
                })()}
              </div>
              {fieldErrorMessage(tokenForm.formState.errors, 'permissionKinds') && (
                <p role="alert" className="mt-2 text-xs text-destructive">
                  {fieldErrorMessage(tokenForm.formState.errors, 'permissionKinds')}
                </p>
              )}
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
              {t('admin.cancel')}
            </Button>
            <Button onClick={handleCreate} disabled={!canCreate}>
              {mintTokenMutation.isPending ? t('admin.creating') : t('admin.create')}
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
