import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  LoaderCircle,
  Plus,
  Search,
  SearchX,
  ShieldAlert,
  UserCog,
  UserRound,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { Catalog, queries } from '@/shared/api'
import type {
  CatalogLibraryResponse,
  IamPermissionKind,
  SetUserAccessRequest,
  SystemRole,
  UserResponse,
} from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { errorMessage } from '@/shared/lib/errorMessage'
import { screenPhase } from '@/shared/lib/screenPhase'
import { Badge, Button, ErrorState, Modal, PageHeader, Panel, SearchBox } from '@/shared/ui'
import { errorStatus, userAccessOptions, useUserMutations } from './api/users'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

const ROLES: readonly SystemRole[] = ['viewer', 'operator', 'admin']
const WORKSPACE_PERMISSIONS = ['none', 'workspace_read', 'workspace_admin'] as const
const LIBRARY_PERMISSIONS = ['none', 'library_read', 'library_write'] as const
const LIBRARY_PAGE_SIZE = 200

type AccessPermission = IamPermissionKind | 'none'
type CreateUserForm = {
  displayName: string
  email: string
  login: string
  password: string
  role: SystemRole
}

const EMPTY_USER_FORM: CreateUserForm = {
  displayName: '',
  email: '',
  login: '',
  password: '',
  role: 'viewer',
}

function initials(displayName: string): string {
  return displayName
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? '')
    .join('')
}

type WorkspaceLibraries = Readonly<{
  complete: boolean
  items: CatalogLibraryResponse[]
  totalCount: number
  workspaceId: string
}>

function ManageAccessDialog({
  user,
  onClose,
}: Readonly<{ user: UserResponse; onClose: () => void }>) {
  const { t } = useTranslation()
  const { setUserAccess } = useUserMutations()
  const [draftOverrides, setDraftOverrides] = useState<Record<string, AccessPermission>>({})
  const [expandedWorkspaceIds, setExpandedWorkspaceIds] = useState<Set<string>>(() => new Set())
  const [resourceSearch, setResourceSearch] = useState('')
  const [stale, setStale] = useState(false)
  const workspacesQuery = useQuery(queries.listCatalogWorkspacesOptions())
  const workspaces = workspacesQuery.data ?? []
  const librariesQuery = useQuery({
    queryKey: ['admin', 'user-access-libraries', workspaces.map((workspace) => workspace.id)],
    enabled: workspacesQuery.isSuccess,
    queryFn: async (): Promise<WorkspaceLibraries[]> =>
      Promise.all(
        workspaces.map(async (workspace) => {
          const items: CatalogLibraryResponse[] = []
          let offset = 0
          let totalCount: number
          do {
            const response = await Catalog.listCatalogLibraries({
              path: { workspaceId: workspace.id },
              query: { limit: LIBRARY_PAGE_SIZE, offset },
              throwOnError: true,
            })
            const page = response.data
            items.push(...page.items)
            totalCount = page.totalCount
            offset += page.items.length
            if (page.items.length === 0) break
          } while (offset < totalCount)
          return {
            workspaceId: workspace.id,
            items,
            totalCount,
            complete: items.length >= totalCount,
          }
        }),
      ),
  })
  const accessQuery = useQuery(userAccessOptions(user.principalId))

  const initialDraft = useMemo<Record<string, AccessPermission>>(() => {
    const access = accessQuery.data?.access
    if (!access) return {}
    return {
      ...Object.fromEntries(
        access.workspaces.map((entry) => [entry.workspaceId, entry.permissionKind]),
      ),
      ...Object.fromEntries(
        access.libraries.map((entry) => [entry.libraryId, entry.permissionKind]),
      ),
    }
  }, [accessQuery.data])
  const draft = { ...initialDraft, ...draftOverrides }

  const librariesByWorkspace = useMemo(
    () => new Map((librariesQuery.data ?? []).map((entry) => [entry.workspaceId, entry] as const)),
    [librariesQuery.data],
  )
  const incompleteLibraryLoad = (librariesQuery.data ?? []).some((entry) => !entry.complete)
  const isLoading = workspacesQuery.isLoading || accessQuery.isLoading || librariesQuery.isLoading
  const loadError = workspacesQuery.error ?? accessQuery.error ?? librariesQuery.error
  const accessPhase = screenPhase({ loading: isLoading, failed: Boolean(loadError) })

  const save = () => {
    if (incompleteLibraryLoad) return
    const workspaceGrants = new Map(
      (accessQuery.data?.access.workspaces ?? []).map((entry) => [
        entry.workspaceId,
        entry.permissionKind,
      ]),
    )
    const libraryGrants = new Map(
      (accessQuery.data?.access.libraries ?? []).map((entry) => [
        entry.libraryId,
        entry.permissionKind,
      ]),
    )
    workspaces.forEach((workspace) => {
      const permission = draft[workspace.id] ?? 'none'
      if (permission === 'none') workspaceGrants.delete(workspace.id)
      else workspaceGrants.set(workspace.id, permission)
    })
    ;(librariesQuery.data ?? []).forEach((workspaceLibraries) => {
      workspaceLibraries.items.forEach((library) => {
        const permission = draft[library.id] ?? 'none'
        if (permission === 'none') libraryGrants.delete(library.id)
        else libraryGrants.set(library.id, permission)
      })
    })
    const body: SetUserAccessRequest = {
      workspaces: [...workspaceGrants].map(([workspaceId, permissionKind]) => ({
        workspaceId,
        permissionKind,
      })),
      libraries: [...libraryGrants].map(([libraryId, permissionKind]) => ({
        libraryId,
        permissionKind,
      })),
    }
    setUserAccess.mutate(
      { userId: user.principalId, body, etag: accessQuery.data?.etag ?? null },
      {
        onSuccess: () => {
          toast.success(t('admin.users.access.saved', { name: user.displayName }))
          onClose()
        },
        onError: (error) => {
          if (errorStatus(error) === 409) {
            setStale(true)
            return
          }
          toast.error(errorMessage(error, t('admin.users.access.saveFailed')))
        },
      },
    )
  }

  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      title={`${t('admin.manageAccess')} · ${user.displayName}`}
      description={t('admin.users.access.etagDescription')}
      wide
      footer={
        <>
          <Button onClick={onClose}>{t('common.cancel')}</Button>
          <Button
            variant="primary"
            disabled={
              isLoading || Boolean(loadError) || incompleteLibraryLoad || setUserAccess.isPending
            }
            onClick={save}
          >
            {setUserAccess.isPending ? t('admin.saving') : t('common.save')}
          </Button>
        </>
      }
    >
      {accessPhase === 'loading' && (
        <div className="state-card aicfg-loading-state">
          <LoaderCircle className="spin" size={22} />
          <strong>{t('admin.users.access.loading')}</strong>
        </div>
      )}
      {accessPhase === 'error' && (
        <div className="state-card state-card--error">
          <AlertTriangle size={22} />
          <div>
            <strong>{t('admin.users.access.loadFailed')}</strong>
            <Button
              size="sm"
              onClick={() => {
                void workspacesQuery.refetch()
                void accessQuery.refetch()
                void librariesQuery.refetch()
              }}
            >
              {t('common.tryAgain')}
            </Button>
          </div>
        </div>
      )}
      {accessPhase === 'ready' && (
        <div className="access-matrix aicfg-access-matrix">
          <div className="search-box aicfg-access-search">
            <Search size={16} />
            <input
              className="input"
              value={resourceSearch}
              onChange={(event) => setResourceSearch(event.target.value)}
              placeholder={t('admin.users.access.searchPlaceholder')}
            />
          </div>
          <header>
            <span>{t('admin.users.access.resource')}</span>
            <span>{t('admin.users.access.permission')}</span>
          </header>
          {workspaces.map((workspace) => {
            const workspaceLibraries = librariesByWorkspace.get(workspace.id)
            const normalizedSearch = resourceSearch.trim().toLocaleLowerCase()
            const matchingLibraries = (workspaceLibraries?.items ?? []).filter(
              (library) =>
                !normalizedSearch ||
                library.displayName.toLocaleLowerCase().includes(normalizedSearch),
            )
            const workspaceMatches = workspace.displayName
              .toLocaleLowerCase()
              .includes(normalizedSearch)
            if (normalizedSearch && !workspaceMatches && matchingLibraries.length === 0) return null
            const expanded = expandedWorkspaceIds.has(workspace.id) || Boolean(normalizedSearch)
            return (
              <div className="access-resource-group" key={workspace.id}>
                <div className="access-resource-row">
                  <button
                    type="button"
                    className="aicfg-resource-toggle"
                    aria-expanded={expanded}
                    onClick={() =>
                      setExpandedWorkspaceIds((current) => {
                        const next = new Set(current)
                        if (next.has(workspace.id)) next.delete(workspace.id)
                        else next.add(workspace.id)
                        return next
                      })
                    }
                  >
                    {expanded ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
                    <strong>{workspace.displayName}</strong>
                    <small>{workspaceLibraries?.totalCount ?? 0}</small>
                  </button>
                  <select
                    className="input"
                    value={draft[workspace.id] ?? 'none'}
                    onChange={(event) =>
                      setDraftOverrides((current) => ({
                        ...current,
                        [workspace.id]: event.target.value as AccessPermission,
                      }))
                    }
                  >
                    {WORKSPACE_PERMISSIONS.map((permission) => (
                      <option key={permission} value={permission}>
                        {t(`admin.permissionLabels.${permission}`)}
                      </option>
                    ))}
                  </select>
                </div>
                {expanded &&
                  matchingLibraries.map((library) => (
                    <div
                      className="access-resource-row access-resource-row--library"
                      key={library.id}
                    >
                      <span>{library.displayName}</span>
                      <select
                        className="input"
                        value={draft[library.id] ?? 'none'}
                        onChange={(event) =>
                          setDraftOverrides((current) => ({
                            ...current,
                            [library.id]: event.target.value as AccessPermission,
                          }))
                        }
                      >
                        {LIBRARY_PERMISSIONS.map((permission) => (
                          <option key={permission} value={permission}>
                            {t(`admin.permissionLabels.${permission}`)}
                          </option>
                        ))}
                      </select>
                    </div>
                  ))}
              </div>
            )
          })}
        </div>
      )}
      {incompleteLibraryLoad && (
        <div className="inline-alert">
          <AlertTriangle size={15} />
          <span>{t('admin.users.access.incompleteLibraries')}</span>
        </div>
      )}
      {stale && (
        <div className="inline-alert">
          <ShieldAlert size={15} />
          <span>{t('admin.users.access.stale')}</span>
          <Button
            size="sm"
            onClick={() => {
              setDraftOverrides({})
              setStale(false)
              void accessQuery.refetch()
            }}
          >
            {t('admin.users.access.reload')}
          </Button>
        </div>
      )}
    </Modal>
  )
}

function CreateUserDialog({
  open,
  onOpenChange,
}: Readonly<{ open: boolean; onOpenChange: (open: boolean) => void }>) {
  const { t } = useTranslation()
  const { createUser } = useUserMutations()
  const [form, setForm] = useState<CreateUserForm>(EMPTY_USER_FORM)
  const loginValid = /^[a-z0-9._-]+$/.test(form.login)
  const emailValid = /^[^\s@]+@[^\s@.]+(?:\.[^\s@.]+)+$/.test(form.email)
  const valid =
    loginValid && emailValid && Boolean(form.displayName.trim()) && form.password.length >= 8

  const close = (nextOpen: boolean) => {
    if (!nextOpen) setForm(EMPTY_USER_FORM)
    onOpenChange(nextOpen)
  }

  const create = () => {
    if (!valid) return
    createUser.mutate(
      {
        body: {
          ...form,
          displayName: form.displayName.trim(),
          email: form.email.trim(),
          login: form.login.trim(),
        },
      },
      {
        onSuccess: (created) => {
          toast.success(t('admin.users.created', { login: created.login }))
          close(false)
        },
        onError: (error) => toast.error(errorMessage(error, t('admin.users.createFailed'))),
      },
    )
  }

  return (
    <Modal
      open={open}
      onOpenChange={close}
      title={t('admin.users.createUserTitle')}
      description={t('admin.users.createUserDesc')}
      footer={
        <>
          <Button onClick={() => close(false)}>{t('common.cancel')}</Button>
          <Button variant="primary" disabled={!valid || createUser.isPending} onClick={create}>
            {createUser.isPending ? t('admin.creating') : t('common.create')}
          </Button>
        </>
      }
    >
      <div className="field-grid">
        <label className="field">
          <span>{t('admin.users.loginLabel')}</span>
          <input
            className="input"
            value={form.login}
            pattern="[a-z0-9._-]+"
            onChange={(event) => setForm((current) => ({ ...current, login: event.target.value }))}
          />
          {form.login && !loginValid && (
            <small className="field-error">{t('admin.users.loginInvalid')}</small>
          )}
        </label>
        <label className="field">
          <span>{t('common.role')}</span>
          <select
            className="input"
            value={form.role}
            onChange={(event) =>
              setForm((current) => ({ ...current, role: event.target.value as SystemRole }))
            }
          >
            {ROLES.map((role) => (
              <option key={role} value={role}>
                {t(`admin.users.roles.${role}`)}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>{t('admin.users.emailLabel')}</span>
          <input
            className="input"
            type="email"
            value={form.email}
            onChange={(event) => setForm((current) => ({ ...current, email: event.target.value }))}
          />
        </label>
        <label className="field">
          <span>{t('admin.users.displayNameLabel')}</span>
          <input
            className="input"
            value={form.displayName}
            onChange={(event) =>
              setForm((current) => ({ ...current, displayName: event.target.value }))
            }
          />
        </label>
        <label className="field field--wide">
          <span>{t('admin.users.passwordLabel')}</span>
          <input
            className="input"
            type="password"
            minLength={8}
            value={form.password}
            onChange={(event) =>
              setForm((current) => ({ ...current, password: event.target.value }))
            }
          />
        </label>
      </div>
    </Modal>
  )
}

export default function UsersAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const { user: currentUser } = useApp()
  const { deleteUser, patchUser } = useUserMutations()
  const [createOpen, setCreateOpen] = useState(false)
  const [accessUser, setAccessUser] = useState<UserResponse | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<UserResponse | null>(null)
  const [search, setSearch] = useState('')
  const [roleFilter, setRoleFilter] = useState<SystemRole | 'all'>('all')
  const usersQuery = useQuery(queries.listIamUsersOptions())
  const usersPhase = screenPhase({ loading: usersQuery.isLoading, failed: usersQuery.isError })
  const users = useMemo(() => usersQuery.data ?? [], [usersQuery.data])
  const filtered = useMemo(
    () =>
      users.filter((entry) => {
        const matchesRole = roleFilter === 'all' || entry.role === roleFilter
        const needle = search.trim().toLowerCase()
        return (
          matchesRole &&
          (!needle ||
            `${entry.displayName} ${entry.login} ${entry.email}`.toLowerCase().includes(needle))
        )
      }),
    [roleFilter, search, users],
  )

  const showLastAdminError = () => toast.error(t('admin.lastAdmin'))
  const changeRole = (entry: UserResponse, role: SystemRole) => {
    if (entry.principalId === currentUser?.id) return
    patchUser.mutate(
      { path: { userId: entry.principalId }, body: { role } },
      {
        onSuccess: () => toast.success(t('admin.users.roleChanged', { login: entry.login })),
        onError: (error) =>
          errorStatus(error) === 409
            ? showLastAdminError()
            : toast.error(errorMessage(error, t('admin.users.roleChangeFailed'))),
      },
    )
  }

  const confirmDelete = () => {
    if (!deleteTarget || deleteTarget.principalId === currentUser?.id) return
    deleteUser.mutate(
      { path: { userId: deleteTarget.principalId } },
      {
        onSuccess: () => {
          toast.success(t('admin.users.deleted', { login: deleteTarget.login }))
          setDeleteTarget(null)
        },
        onError: (error) =>
          errorStatus(error) === 409
            ? showLastAdminError()
            : toast.error(errorMessage(error, t('admin.users.deleteFailed'))),
      },
    )
  }

  return (
    <div className="aicfg-readable-labels">
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.usersTitle')}
        description={t('admin.usersSubtitlePolished')}
        actions={
          <Button variant="primary" onClick={() => setCreateOpen(true)}>
            <Plus size={15} />
            {t('common.create')}
          </Button>
        }
      />
      {usersPhase === 'loading' && (
        <div className="state-card aicfg-loading-state">
          <LoaderCircle className="spin" size={22} />
          <strong>{t('common.loading')}</strong>
        </div>
      )}
      {usersPhase === 'error' && (
        <ErrorState title={t('admin.users.loadFailed')} onRetry={() => void usersQuery.refetch()} />
      )}
      {usersPhase === 'ready' && (
        <>
          <div className="toolbar">
            <SearchBox value={search} onChange={setSearch} />
            <select
              className="input compact-input"
              value={roleFilter}
              onChange={(event) => setRoleFilter(event.target.value as SystemRole | 'all')}
            >
              <option value="all">{t('common.all')}</option>
              {ROLES.map((role) => (
                <option key={role} value={role}>
                  {t(`admin.users.roles.${role}`)}
                </option>
              ))}
            </select>
          </div>
          {filtered.length === 0 ? (
            <div className="state-card state-card--stacked">
              {users.length === 0 ? <UserRound size={26} /> : <SearchX size={26} />}
              <div>
                <strong>
                  {users.length === 0 ? t('admin.users.empty') : t('common.noMatches')}
                </strong>
              </div>
            </div>
          ) : (
            <Panel>
              <div className="user-list">
                {filtered.map((entry) => {
                  const isCurrentUser = entry.principalId === currentUser?.id
                  const ownAccountTitle = isCurrentUser
                    ? t('admin.users.ownAccountLocked')
                    : undefined
                  return (
                    <article className="aicfg-user-row" key={entry.principalId}>
                      <span className="user-avatar">{initials(entry.displayName)}</span>
                      <div>
                        <strong>
                          {entry.displayName}
                          {isCurrentUser && <Badge tone="ready">{t('admin.thisIsYou')}</Badge>}
                        </strong>
                        <small>
                          {entry.login} · {entry.email}
                        </small>
                      </div>
                      <select
                        aria-label={t('admin.users.changeRole')}
                        className="input aicfg-user-role"
                        value={entry.role}
                        disabled={patchUser.isPending || isCurrentUser}
                        title={ownAccountTitle}
                        onChange={(event) => changeRole(entry, event.target.value as SystemRole)}
                      >
                        {ROLES.map((role) => (
                          <option key={role} value={role}>
                            {t(`admin.users.roles.${role}`)}
                          </option>
                        ))}
                      </select>
                      <Button size="sm" onClick={() => setAccessUser(entry)}>
                        <UserCog size={14} />
                        {t('admin.manageAccess')}
                      </Button>
                      <Button
                        size="sm"
                        variant="danger"
                        disabled={deleteUser.isPending || isCurrentUser}
                        title={ownAccountTitle}
                        onClick={() => setDeleteTarget(entry)}
                      >
                        {t('common.delete')}
                      </Button>
                    </article>
                  )
                })}
              </div>
            </Panel>
          )}
        </>
      )}
      <CreateUserDialog open={createOpen} onOpenChange={setCreateOpen} />
      {accessUser && <ManageAccessDialog user={accessUser} onClose={() => setAccessUser(null)} />}
      <Modal
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        title={t('admin.users.deleteTitle')}
        description={t('admin.users.deleteDescription', { login: deleteTarget?.login ?? '' })}
        footer={
          <>
            <Button onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
            <Button variant="danger" disabled={deleteUser.isPending} onClick={confirmDelete}>
              {t('common.delete')}
            </Button>
          </>
        }
      >
        <div className="inline-alert">
          <ShieldAlert size={15} />
          {t('admin.users.deleteWarning')}
        </div>
      </Modal>
    </div>
  )
}
