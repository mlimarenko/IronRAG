import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';
import { ChevronDown, ChevronRight, Library, Loader2, Building2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { adminApi } from '@/shared/api';
import type {
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  SetUserAccessRequest,
  UserResponse,
} from '@/shared/api/admin';
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
import { ScrollArea } from '@/shared/components/ui/scroll-area';
import { Separator } from '@/shared/components/ui/separator';
import { errorMessage } from '@/shared/lib/errorMessage';

interface UserAccessDialogProps {
  /** The user whose access is being edited, or `null` when the dialog is closed. */
  user: UserResponse | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * Per-user access editor (admin-only, gated upstream by `useCan('users.manage')`).
 *
 * Shows every workspace with its libraries nested underneath; the admin checks a
 * workspace to grant the user access to it (which lets an operator author
 * libraries there and a viewer read it), and checks individual libraries to
 * grant library-scoped access. On save the full desired set is sent to
 * `PUT /v1/iam/users/{id}/access`, which reconciles grants server-side. The
 * system role (separate control) decides read-vs-write capability; this editor
 * only decides *which* workspaces/libraries the user can reach.
 */
export function UserAccessDialog({ user, open, onOpenChange }: UserAccessDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const principalId = user?.principalId ?? null;

  const workspacesQuery = useQuery({
    queryKey: ['admin', 'iam', 'access', 'workspaces'],
    queryFn: () => adminApi.listWorkspaces(),
    enabled: open,
  });
  const workspaces = useMemo<CatalogWorkspaceResponse[]>(
    () => (Array.isArray(workspacesQuery.data) ? workspacesQuery.data : []),
    [workspacesQuery.data],
  );

  const accessQuery = useQuery({
    queryKey: ['admin', 'iam', 'access', principalId],
    queryFn: () => adminApi.getUserAccess(principalId as string),
    enabled: open && principalId != null,
  });

  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectedWorkspaces, setSelectedWorkspaces] = useState<Set<string>>(new Set());
  const [selectedLibraries, setSelectedLibraries] = useState<Set<string>>(new Set());

  // Seed the selection from the loaded access whenever a fresh user/access loads.
  useEffect(() => {
    if (!accessQuery.data) return;
    setSelectedWorkspaces(new Set(accessQuery.data.workspaces.map((entry) => entry.workspaceId)));
    setSelectedLibraries(new Set(accessQuery.data.libraries.map((entry) => entry.libraryId)));
    // Auto-expand workspaces that already have library grants so they're visible.
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const entry of accessQuery.data.libraries) next.add(entry.workspaceId);
      return next;
    });
  }, [accessQuery.data]);

  // Reset local state when the dialog closes so a re-open starts clean.
  useEffect(() => {
    if (!open) {
      setExpanded(new Set());
      setSelectedWorkspaces(new Set());
      setSelectedLibraries(new Set());
    }
  }, [open]);

  // Lazily load libraries only for expanded workspaces.
  const expandedIds = useMemo(() => [...expanded], [expanded]);
  const libraryQueries = useQueries({
    queries: expandedIds.map((workspaceId) => ({
      queryKey: ['admin', 'iam', 'access', 'libraries', workspaceId],
      queryFn: () => adminApi.listLibraries(workspaceId),
      enabled: open,
    })),
  });
  const librariesByWorkspace = useMemo(() => {
    const map = new Map<string, CatalogLibraryResponse[]>();
    expandedIds.forEach((workspaceId, index) => {
      const data = libraryQueries[index]?.data;
      map.set(workspaceId, Array.isArray(data) ? data : []);
    });
    return map;
  }, [expandedIds, libraryQueries]);

  const saveMutation = useMutation({
    mutationKey: ['admin', 'iam', 'access', 'save'],
    mutationFn: (request: { principalId: string; body: SetUserAccessRequest }) =>
      adminApi.setUserAccess(request.principalId, request.body),
    onSuccess: () => {
      toast.success(t('admin.users.access.saved', { name: user?.displayName ?? '' }));
      if (principalId != null) {
        void queryClient.invalidateQueries({
          queryKey: ['admin', 'iam', 'access', principalId],
        });
      }
      onOpenChange(false);
    },
    onError: (err) => {
      toast.error(errorMessage(err, t('admin.users.access.saveFailed')));
    },
  });

  function toggleExpanded(workspaceId: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(workspaceId)) next.delete(workspaceId);
      else next.add(workspaceId);
      return next;
    });
  }

  function toggleWorkspace(workspaceId: string, checked: boolean) {
    setSelectedWorkspaces((prev) => {
      const next = new Set(prev);
      if (checked) next.add(workspaceId);
      else next.delete(workspaceId);
      return next;
    });
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    setSelectedLibraries((prev) => {
      const next = new Set(prev);
      if (checked) next.add(libraryId);
      else next.delete(libraryId);
      return next;
    });
  }

  function handleSave() {
    if (principalId == null) return;
    const body: SetUserAccessRequest = {
      workspaces: [...selectedWorkspaces].map((workspaceId) => ({
        workspaceId,
        permissionKind: 'workspace_read',
      })),
      libraries: [...selectedLibraries].map((libraryId) => ({
        libraryId,
        permissionKind: 'library_read',
      })),
    };
    saveMutation.mutate({ principalId, body });
  }

  const isLoading = workspacesQuery.isLoading || accessQuery.isLoading;
  const loadError = workspacesQuery.error ?? accessQuery.error;
  const grantedCount = selectedWorkspaces.size + selectedLibraries.size;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {t('admin.users.access.title', { name: user?.displayName ?? '' })}
          </DialogTitle>
          <DialogDescription>{t('admin.users.access.description')}</DialogDescription>
        </DialogHeader>

        {isLoading ? (
          <div className="flex items-center justify-center gap-2 py-10 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" /> {t('admin.users.access.loading')}
          </div>
        ) : loadError ? (
          <div className="py-8 text-center text-sm text-status-failed">
            {errorMessage(loadError, t('admin.users.access.loadFailed'))}
          </div>
        ) : workspaces.length === 0 ? (
          <div className="py-10 text-center text-sm text-muted-foreground">
            {t('admin.users.access.empty')}
          </div>
        ) : (
          <ScrollArea className="max-h-[55vh] pr-3">
            <div className="space-y-1.5">
              {workspaces.map((workspace) => {
                const isExpanded = expanded.has(workspace.id);
                const libraries = librariesByWorkspace.get(workspace.id) ?? [];
                const libsLoading =
                  isExpanded &&
                  libraryQueries[expandedIds.indexOf(workspace.id)]?.isLoading === true;
                return (
                  <div key={workspace.id} className="workbench-surface overflow-hidden">
                    <div className="flex items-center gap-2.5 p-3">
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() => toggleExpanded(workspace.id)}
                        aria-label={t('admin.users.access.toggleLibraries')}
                      >
                        {isExpanded ? (
                          <ChevronDown className="h-4 w-4" />
                        ) : (
                          <ChevronRight className="h-4 w-4" />
                        )}
                      </button>
                      <Building2 className="h-4 w-4 shrink-0 text-muted-foreground" />
                      <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5">
                        <Checkbox
                          checked={selectedWorkspaces.has(workspace.id)}
                          onCheckedChange={(checked) =>
                            toggleWorkspace(workspace.id, checked === true)
                          }
                        />
                        <span className="truncate text-sm font-semibold">
                          {workspace.displayName}
                        </span>
                      </label>
                    </div>

                    {isExpanded && (
                      <>
                        <Separator />
                        <div className="space-y-1 bg-surface-sunken/40 py-2 pl-12 pr-3">
                          {libsLoading ? (
                            <div className="flex items-center gap-2 py-1.5 text-xs text-muted-foreground">
                              <Loader2 className="h-3 w-3 animate-spin" />
                              {t('admin.users.access.librariesLoading')}
                            </div>
                          ) : libraries.length === 0 ? (
                            <div className="py-1.5 text-xs text-muted-foreground">
                              {t('admin.users.access.librariesEmpty')}
                            </div>
                          ) : (
                            libraries.map((library) => (
                              <label
                                key={library.id}
                                className="flex cursor-pointer items-center gap-2.5 py-1"
                              >
                                <Checkbox
                                  checked={selectedLibraries.has(library.id)}
                                  onCheckedChange={(checked) =>
                                    toggleLibrary(library.id, checked === true)
                                  }
                                />
                                <Library className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                                <span className="truncate text-sm">{library.displayName}</span>
                              </label>
                            ))
                          )}
                        </div>
                      </>
                    )}
                  </div>
                );
              })}
            </div>
          </ScrollArea>
        )}

        <DialogFooter className="items-center sm:justify-between">
          <span className="text-xs text-muted-foreground">
            {t('admin.users.access.grantedCount', { count: grantedCount })}
          </span>
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              {t('admin.cancel')}
            </Button>
            <Button onClick={handleSave} disabled={isLoading || saveMutation.isPending}>
              {saveMutation.isPending ? t('admin.saving') : t('admin.save')}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
