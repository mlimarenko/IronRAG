import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';
import { z } from 'zod';
import { KeyRound, Loader2, ShieldCheck, UserPlus, Users } from 'lucide-react';

import { adminApi, queries } from '@/shared/api';
import type { SystemRole, UserResponse } from '@/shared/api/admin';
import { UserAccessDialog } from './UserAccessDialog';
import { useApp } from '@/shared/contexts/app-context';
import { useCan } from '@/shared/auth/useCan';
import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { errorMessage } from '@/shared/lib/errorMessage';
import {
  FormInputField,
  FormSelectField,
  nonEmptyString,
  useTypedForm,
} from '@/shared/forms';
import { useTranslation } from 'react-i18next';

const SYSTEM_ROLES: readonly SystemRole[] = ['viewer', 'operator', 'admin'];

/**
 * `/admin/users` — user management (§8 of the 0.5.0 UX plan, gated to
 * `users.manage`). Lists users, creates new ones and changes their system
 * role against the admin-only `/v1/iam/users` endpoints. The route is only
 * registered for admins (see AdminPage), so this surface assumes the caller
 * already holds `users.manage`; the backend re-enforces it regardless.
 */
export default function AdminUsersPage() {
  const { t } = useTranslation();
  const { user } = useApp();
  const { can } = useCan();
  const canManage = can('users.manage');
  const queryClient = useQueryClient();

  const usersListQuery = queries.listIamUsersOptions();
  const usersQuery = useQuery({ ...usersListQuery, enabled: canManage });
  const users = useMemo<UserResponse[]>(
    () => (Array.isArray(usersQuery.data) ? usersQuery.data : []),
    [usersQuery.data],
  );

  const [createOpen, setCreateOpen] = useState(false);
  const [accessUser, setAccessUser] = useState<UserResponse | null>(null);

  const createSchema = useMemo(
    () =>
      z.object({
        login: nonEmptyString(t('admin.users.loginLabel')),
        email: nonEmptyString(t('admin.users.emailLabel')),
        displayName: nonEmptyString(t('admin.users.displayNameLabel')),
        password: z.string().min(8, t('admin.users.passwordTooShort')),
        role: z.enum(['viewer', 'operator', 'admin']),
      }),
    [t],
  );
  const createForm = useTypedForm({
    schema: createSchema,
    defaultValues: {
      login: '',
      email: '',
      displayName: '',
      password: '',
      role: 'viewer',
    },
    mode: 'onChange',
  });

  const createUserMutation = useMutation({
    mutationKey: ['admin', 'iam', 'users', 'create'],
    mutationFn: (request: {
      login: string;
      email: string;
      displayName: string;
      password: string;
      role: SystemRole;
    }) => adminApi.createUser(request),
    onSuccess: (created) => {
      toast.success(t('admin.users.created', { login: created.login }));
      setCreateOpen(false);
      createForm.reset({
        login: '',
        email: '',
        displayName: '',
        password: '',
        role: 'viewer',
      });
      void queryClient.invalidateQueries({ queryKey: usersListQuery.queryKey });
    },
    onError: (err) => {
      toast.error(errorMessage(err, t('admin.users.createFailed')));
    },
  });

  const setRoleMutation = useMutation({
    mutationKey: ['admin', 'iam', 'users', 'setRole'],
    mutationFn: ({ principalId, role }: { principalId: string; role: SystemRole }) =>
      adminApi.setUserRole(principalId, role),
    onSuccess: (updated) => {
      toast.success(t('admin.users.roleChanged', { login: updated.login }));
      void queryClient.invalidateQueries({ queryKey: usersListQuery.queryKey });
    },
    onError: (err) => {
      toast.error(errorMessage(err, t('admin.users.roleChangeFailed')));
    },
  });

  const handleCreate = createForm.submitWithMutation(
    {
      mutateAsync: async (values) =>
        createUserMutation.mutateAsync({
          login: values.login.trim(),
          email: values.email.trim(),
          displayName: values.displayName.trim(),
          password: values.password,
          role: values.role,
        }),
    },
    { errorMessage: (err) => errorMessage(err, t('admin.users.createFailed')) },
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-auto p-6">
      <div className="mb-5 flex items-center gap-3">
        <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-primary/10 text-primary">
          <Users className="h-4 w-4" />
        </div>
        <div className="min-w-0">
          <h2 className="text-base font-bold tracking-tight">{t('admin.users.title')}</h2>
          <p className="text-sm text-muted-foreground">{t('admin.users.subtitle')}</p>
        </div>
        <Button size="sm" className="ml-auto" onClick={() => setCreateOpen(true)}>
          <UserPlus className="mr-1.5 h-3.5 w-3.5" />
          {t('admin.users.createUser')}
        </Button>
      </div>

      <div className="mb-4 flex gap-4 text-xs font-semibold">
        {usersQuery.isLoading ? (
          <span className="flex items-center gap-1.5 text-muted-foreground">
            <Loader2 className="h-3 w-3 animate-spin" /> {t('admin.users.loading')}
          </span>
        ) : usersQuery.error ? (
          <span className="text-status-failed">
            {errorMessage(usersQuery.error, t('admin.users.loadFailed'))}
          </span>
        ) : (
          <span className="text-muted-foreground">
            {users.length} {t('admin.users.total')}
          </span>
        )}
      </div>

      <div className="max-w-3xl space-y-1.5">
        {!usersQuery.isLoading && !usersQuery.error && users.length === 0 && (
          <div className="p-8 text-center text-sm text-muted-foreground">
            {t('admin.users.empty')}
          </div>
        )}
        {users.map((entry) => {
          const isSelf = entry.principalId === user?.id;
          return (
            <div
              key={entry.principalId}
              className="workbench-surface flex items-center gap-3 p-4"
            >
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-surface-sunken">
                <ShieldCheck className="h-5 w-5 text-muted-foreground" />
              </div>
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-bold">{entry.displayName}</div>
                <div className="truncate text-xs text-muted-foreground">{entry.email}</div>
              </div>
              {entry.role !== 'admin' && (
                <Button
                  variant="outline"
                  size="sm"
                  className="h-9"
                  onClick={() => setAccessUser(entry)}
                >
                  <KeyRound className="mr-1.5 h-3.5 w-3.5" />
                  {t('admin.users.access.manage')}
                </Button>
              )}
              <Select
                value={entry.role}
                onValueChange={(value) =>
                  setRoleMutation.mutate({
                    principalId: entry.principalId,
                    role: value as SystemRole,
                  })
                }
                disabled={setRoleMutation.isPending}
              >
                <SelectTrigger
                  className="h-9 w-40 text-sm"
                  aria-label={t('admin.users.changeRole')}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {SYSTEM_ROLES.map((role) => (
                    <SelectItem key={role} value={role}>
                      {t(`admin.users.roles.${role}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {isSelf && (
                <span className="status-badge status-ready text-[10px] uppercase">
                  {t('admin.users.currentUser')}
                </span>
              )}
            </div>
          );
        })}
      </div>

      <Dialog
        open={createOpen}
        onOpenChange={(open) => {
          setCreateOpen(open);
          if (!open) {
            createForm.reset({
              login: '',
              email: '',
              displayName: '',
              password: '',
              role: 'viewer',
            });
          }
        }}
      >
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{t('admin.users.createUserTitle')}</DialogTitle>
            <DialogDescription>{t('admin.users.createUserDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <FormInputField
              formState={createForm.formState}
              id="admin-user-login"
              label={t('admin.users.loginLabel')}
              name="login"
              registration={createForm.register('login')}
              placeholder={t('admin.users.loginPlaceholder')}
            />
            <FormInputField
              formState={createForm.formState}
              id="admin-user-email"
              label={t('admin.users.emailLabel')}
              name="email"
              registration={createForm.register('email')}
              placeholder={t('admin.users.emailPlaceholder')}
            />
            <FormInputField
              formState={createForm.formState}
              id="admin-user-display-name"
              label={t('admin.users.displayNameLabel')}
              name="displayName"
              registration={createForm.register('displayName')}
              placeholder={t('admin.users.displayNamePlaceholder')}
            />
            <FormInputField
              formState={createForm.formState}
              id="admin-user-password"
              label={t('admin.users.passwordLabel')}
              name="password"
              type="password"
              registration={createForm.register('password')}
              placeholder={t('admin.users.passwordPlaceholder')}
            />
            <FormSelectField
              control={createForm.control}
              formState={createForm.formState}
              id="admin-user-role"
              label={t('admin.users.role')}
              name="role"
            >
              {SYSTEM_ROLES.map((role) => (
                <SelectItem key={role} value={role}>
                  {t(`admin.users.roles.${role}`)}
                </SelectItem>
              ))}
            </FormSelectField>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
              {t('admin.cancel')}
            </Button>
            <Button
              onClick={handleCreate}
              disabled={!createForm.formState.isValid || createUserMutation.isPending}
            >
              {createUserMutation.isPending ? t('admin.creating') : t('admin.create')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <UserAccessDialog
        user={accessUser}
        open={accessUser != null}
        onOpenChange={(open) => {
          if (!open) setAccessUser(null);
        }}
      />
    </div>
  );
}
