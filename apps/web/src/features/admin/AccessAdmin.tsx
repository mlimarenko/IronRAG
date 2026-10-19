import { useQuery } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import {
  AlertTriangle,
  Copy,
  Eye,
  EyeOff,
  KeyRound,
  LoaderCircle,
  Plus,
  Trash2,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { queries } from '@/shared/api'
import type {
  CatalogLibraryResponse,
  IamPermissionKind,
  TokenResponse,
} from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import {
  DataGrid,
  useDataGridState,
  type GridColumn,
  type GridServiceColumn,
} from '@/shared/data-grid'
import { formatDate, formatRelative } from '@/shared/format'
import { tokenStatusLabel } from '@/shared/lib/enumLabels'
import { errorMessage } from '@/shared/lib/errorMessage'
import type { APIToken } from '@/shared/types'
import { Badge, Button, Drawer, Modal, PageHeader, Panel } from '@/shared/ui'
import { useAccessMutations } from './api/access'
import {
  dateInputFromExpiresAt,
  expiresAtFromDateInput,
  mapAdminToken,
  permissionsForScope,
  type TokenExpiry,
  type TokenScope,
} from './model/accessModel'
import { buildMintTokenRequest } from './model/tokenMint'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

type MintForm = {
  expiry: TokenExpiry
  label: string
  libraryIds: string[]
  permissions: IamPermissionKind[]
  scope: TokenScope
  workspaceId: string
}

const EMPTY_MINT_FORM: MintForm = {
  expiry: '90',
  label: '',
  libraryIds: [],
  permissions: [],
  scope: 'workspace',
  workspaceId: '',
}

const tokenRowId = (token: APIToken) => token.id

function statusTone(status: APIToken['status']): string {
  if (status === 'active') return 'active'
  if (status === 'expired') return 'warning'
  return 'revoked'
}

function tokenScopeLabel(token: APIToken, t: TFunction): string {
  if (token.scope.kind === 'system') return t('admin.system')
  if (token.scope.kind === 'workspace') {
    return token.scope.workspace
      ? `${t('common.workspace')} · ${token.scope.workspace.displayName}`
      : t('common.workspace')
  }
  const names = token.scope.libraries.map((library) => library.displayName).join(', ')
  return names ? `${t('common.library')} · ${names}` : t('common.library')
}

/**
 * What the token table indexes for a date column: the ISO value the API stored
 * *and* the date the operator sees, so either one finds the row. A null date is
 * searched as its displayed "Never", never as the string "null".
 */
function dateSearchText(raw: string | undefined, display: string): string {
  return raw ? `${raw} ${display}` : display
}

function AccessLoading() {
  const { t } = useTranslation()
  return (
    <div className="state-card aicfg-loading-state" aria-live="polite">
      <LoaderCircle className="spin" size={22} />
      <strong>{t('common.loading')}</strong>
    </div>
  )
}

function TokenEditorForm({
  token,
  onDeleted,
}: Readonly<{ token: APIToken; onDeleted: () => void }>) {
  const { t } = useTranslation()
  const { patchToken, deleteToken } = useAccessMutations()
  const tokenId = token.id
  const [label, setLabel] = useState(token.label)
  const [expiresAt, setExpiresAt] = useState(dateInputFromExpiresAt(token.expiresAt))
  const [confirmDelete, setConfirmDelete] = useState(false)

  const save = () => {
    if (!label.trim()) return
    patchToken.mutate(
      {
        path: { tokenId },
        body: { label: label.trim(), expiresAt: expiresAtFromDateInput(expiresAt) },
      },
      {
        onSuccess: () => toast.success(t('admin.access.tokenUpdated')),
        onError: (error) => toast.error(errorMessage(error, t('admin.access.tokenUpdateFailed'))),
      },
    )
  }

  return (
    <div className="token-inspector">
      <div className="copy-row">
        <code>{token.tokenPrefix}</code>
      </div>
      <dl>
        <div>
          <dt>{t('common.status')}</dt>
          <dd>
            <Badge tone={statusTone(token.status)}>{tokenStatusLabel(token.status, t)}</Badge>
          </dd>
        </div>
        <div>
          <dt>{t('common.scope')}</dt>
          <dd>{tokenScopeLabel(token, t)}</dd>
        </div>
        <div>
          <dt>{t('admin.issuedBy')}</dt>
          <dd>{token.issuedBy?.displayLabel ?? t('admin.system')}</dd>
        </div>
        <div>
          <dt>{t('admin.access.tokenId')}</dt>
          <dd>
            <code>{token.id}</code>
          </dd>
        </div>
      </dl>
      <div className="form-section">
        <label className="field">
          <span>{t('admin.label')}</span>
          <input
            className="input"
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
        </label>
        <label className="field">
          <span>{t('admin.expires')}</span>
          <input
            className="input"
            type="date"
            value={expiresAt}
            onChange={(event) => setExpiresAt(event.target.value)}
          />
        </label>
        <Button variant="primary" disabled={!label.trim() || patchToken.isPending} onClick={save}>
          {patchToken.isPending ? t('admin.saving') : t('common.save')}
        </Button>
      </div>
      <Button
        variant="danger"
        className="danger-wide"
        disabled={deleteToken.isPending}
        onClick={() => setConfirmDelete(true)}
      >
        <Trash2 size={15} />
        {t('admin.revokeDelete')}
      </Button>
      <Modal
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title={t('admin.access.deleteTitle')}
        description={t('admin.access.deleteDescription', { label: token.label })}
        footer={
          <>
            <Button onClick={() => setConfirmDelete(false)}>{t('common.cancel')}</Button>
            <Button
              variant="danger"
              disabled={deleteToken.isPending}
              onClick={() =>
                deleteToken.mutate(
                  { path: { tokenId } },
                  {
                    onSuccess: () => {
                      toast.success(t('admin.access.tokenDeleted'))
                      setConfirmDelete(false)
                      onDeleted()
                    },
                    onError: (error) =>
                      toast.error(errorMessage(error, t('admin.deleteTokenFailed'))),
                  },
                )
              }
            >
              {t('admin.revokeDelete')}
            </Button>
          </>
        }
      >
        <div className="inline-alert">
          <AlertTriangle size={15} />
          {t('admin.access.deleteWarning')}
        </div>
      </Modal>
    </div>
  )
}

function TokenEditor({ tokenId, onDeleted }: Readonly<{ tokenId: string; onDeleted: () => void }>) {
  const { t } = useTranslation()
  const tokenQuery = useQuery({
    ...queries.getIamTokenOptions({ path: { tokenId } }),
    select: mapAdminToken,
  })
  if (tokenQuery.isLoading) return <AccessLoading />
  if (tokenQuery.isError || !tokenQuery.data) {
    return (
      <div className="inline-alert">
        <AlertTriangle size={15} />
        {t('admin.access.detailLoadFailed')}
      </div>
    )
  }
  return (
    <TokenEditorForm
      key={`${tokenQuery.data.id}-${tokenQuery.data.label}-${tokenQuery.data.expiresAt ?? ''}`}
      token={tokenQuery.data}
      onDeleted={onDeleted}
    />
  )
}

function MintTokenDialog({
  open,
  onOpenChange,
}: Readonly<{ open: boolean; onOpenChange: (open: boolean) => void }>) {
  const { t } = useTranslation()
  const { activeWorkspace } = useApp()
  const { mintToken } = useAccessMutations()
  const [form, setForm] = useState<MintForm>({
    ...EMPTY_MINT_FORM,
    workspaceId: activeWorkspace?.id ?? '',
  })
  const [plaintext, setPlaintext] = useState<string | null>(null)
  const [showToken, setShowToken] = useState(true)
  const workspacesQuery = useQuery({
    ...queries.listCatalogWorkspacesOptions(),
    enabled: open && !plaintext,
  })
  const workspaces = workspacesQuery.data ?? []
  const selectedWorkspaceId = form.workspaceId || activeWorkspace?.id || workspaces[0]?.id || ''
  const librariesQuery = useQuery({
    ...queries.listCatalogLibrariesOptions({
      path: { workspaceId: selectedWorkspaceId },
      query: { limit: 200, offset: 0 },
    }),
    enabled: open && !plaintext && form.scope === 'library' && Boolean(selectedWorkspaceId),
  })
  const libraries = librariesQuery.data?.items ?? []
  const availablePermissions = permissionsForScope(form.scope)

  const close = (nextOpen: boolean) => {
    if (!nextOpen) {
      setForm({ ...EMPTY_MINT_FORM, workspaceId: activeWorkspace?.id ?? '' })
      setPlaintext(null)
      setShowToken(true)
    }
    onOpenChange(nextOpen)
  }

  const canMint = Boolean(
    form.label.trim() &&
    selectedWorkspaceId &&
    form.permissions.length > 0 &&
    (form.scope === 'workspace' || form.libraryIds.length > 0),
  )

  const mint = () => {
    if (!canMint) return
    const request = buildMintTokenRequest({
      label: form.label,
      expiryDays: form.expiry,
      scope: form.scope,
      workspaceId: selectedWorkspaceId,
      libraryIds: form.libraryIds,
      permissionKinds: form.permissions,
    })
    mintToken.mutate(
      { body: request },
      {
        onSuccess: (result) => {
          setPlaintext(result.token)
          setShowToken(true)
          toast.success(t('admin.tokenCreated'))
        },
        onError: (error) => toast.error(errorMessage(error, t('admin.createTokenFailed'))),
      },
    )
  }

  return (
    <Modal
      open={open}
      onOpenChange={close}
      title={plaintext ? t('admin.tokenCreated') : t('admin.mint')}
      description={plaintext ? t('admin.tokenCreatedDesc') : t('admin.accessSubtitlePolished')}
      wide
      footer={
        plaintext ? (
          <Button variant="primary" onClick={() => close(false)}>
            {t('admin.done')}
          </Button>
        ) : (
          <>
            <Button onClick={() => close(false)}>{t('common.cancel')}</Button>
            <Button variant="primary" disabled={!canMint || mintToken.isPending} onClick={mint}>
              {mintToken.isPending ? t('admin.creating') : t('admin.mint')}
            </Button>
          </>
        )
      }
    >
      {plaintext ? (
        <div className="token-reveal">
          <KeyRound size={28} />
          <strong>{t('admin.tokenOnce')}</strong>
          <p>{t('admin.access.plaintextWarning', { productName: getBranding().displayName })}</p>
          <div>
            <code>{showToken ? plaintext : '••••••••••••••••••••••••••••••••'}</code>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('admin.access.toggleToken')}
              onClick={() => setShowToken((current) => !current)}
            >
              {showToken ? <EyeOff size={15} /> : <Eye size={15} />}
            </Button>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('common.copy')}
              onClick={() => {
                void navigator.clipboard.writeText(plaintext)
                toast.success(t('common.copied'))
              }}
            >
              <Copy size={15} />
            </Button>
          </div>
        </div>
      ) : (
        <div className="field-grid">
          <label className="field field--wide">
            <span>{t('admin.tokenLabel')}</span>
            <input
              className="input"
              value={form.label}
              onChange={(event) =>
                setForm((current) => ({ ...current, label: event.target.value }))
              }
            />
          </label>
          <label className="field">
            <span>{t('admin.tokenExpiry')}</span>
            <select
              className="input"
              value={form.expiry}
              onChange={(event) =>
                setForm((current) => ({ ...current, expiry: event.target.value as TokenExpiry }))
              }
            >
              <option value="30">{t('admin.tokenExpiry30')}</option>
              <option value="90">{t('admin.tokenExpiry90')}</option>
              <option value="365">{t('admin.tokenExpiry365')}</option>
              <option value="never">{t('admin.never')}</option>
            </select>
          </label>
          <label className="field">
            <span>{t('common.scope')}</span>
            <select
              className="input"
              value={form.scope}
              onChange={(event) => {
                const scope = event.target.value as TokenScope
                setForm((current) => ({
                  ...current,
                  scope,
                  libraryIds: [],
                  permissions: current.permissions.filter((permission) =>
                    permissionsForScope(scope).includes(permission),
                  ),
                }))
              }}
            >
              <option value="workspace">{t('common.workspace')}</option>
              <option value="library">{t('common.library')}</option>
            </select>
          </label>
          <label className="field field--wide">
            <span>{t('common.workspace')}</span>
            <select
              className="input"
              value={selectedWorkspaceId}
              onChange={(event) =>
                setForm((current) => ({
                  ...current,
                  workspaceId: event.target.value,
                  libraryIds: [],
                }))
              }
            >
              {workspaces.map((workspace) => (
                <option key={workspace.id} value={workspace.id}>
                  {workspace.displayName}
                </option>
              ))}
            </select>
          </label>
          {form.scope === 'library' && (
            <label className="field field--wide">
              <span>{t('admin.tokenLibraries')}</span>
              <select
                className="input"
                multiple
                value={form.libraryIds}
                onChange={(event) =>
                  setForm((current) => ({
                    ...current,
                    libraryIds: Array.from(
                      event.currentTarget.selectedOptions,
                      (option) => option.value,
                    ),
                  }))
                }
              >
                {libraries.map((library: CatalogLibraryResponse) => (
                  <option key={library.id} value={library.id}>
                    {library.displayName}
                  </option>
                ))}
              </select>
              {librariesQuery.data && librariesQuery.data.totalCount > libraries.length && (
                <small className="aicfg-library-limit-warning">
                  {t('admin.libraryPicker.partial', {
                    shown: libraries.length,
                    total: librariesQuery.data.totalCount,
                  })}
                </small>
              )}
            </label>
          )}
          <div className="field field--wide">
            <span>{t('admin.tokenPermissions')}</span>
            <div className="permission-grid">
              {availablePermissions.map((permission) => (
                <label key={permission}>
                  <input
                    type="checkbox"
                    checked={form.permissions.includes(permission)}
                    onChange={(event) =>
                      setForm((current) => ({
                        ...current,
                        permissions: event.target.checked
                          ? [...current.permissions, permission]
                          : current.permissions.filter((candidate) => candidate !== permission),
                      }))
                    }
                  />
                  {t(`admin.permissionLabels.${permission}`)}
                </label>
              ))}
            </div>
          </div>
          {!canMint && (
            <div className="inline-alert">
              <AlertTriangle size={15} />
              {t('admin.access.completeMintForm')}
            </div>
          )}
        </div>
      )}
    </Modal>
  )
}

function useTokenColumns(): GridColumn<APIToken>[] {
  const { t } = useTranslation()
  const { locale } = useApp()
  return useMemo(
    (): GridColumn<APIToken>[] => [
      {
        id: 'label',
        label: t('admin.tokenLabel'),
        value: (token) => token.label,
        searchText: (token) => `${token.label} ${token.tokenPrefix}`,
        cell: (token) => (
          <div className="token-name">
            <KeyRound size={15} aria-hidden="true" />
            <div className="cell-title">
              <strong>{token.label}</strong>
              <code>{token.tokenPrefix}</code>
            </div>
          </div>
        ),
      },
      {
        id: 'scope',
        label: t('common.scope'),
        value: (token) => tokenScopeLabel(token, t),
      },
      {
        id: 'status',
        label: t('common.status'),
        value: (token) => token.status,
        displayText: (token) => tokenStatusLabel(token.status, t),
        cell: (token) => (
          <Badge tone={statusTone(token.status)}>{tokenStatusLabel(token.status, t)}</Badge>
        ),
      },
      {
        id: 'issuedBy',
        label: t('admin.issuedBy'),
        value: (token) => token.issuedBy?.displayLabel ?? null,
        displayText: (token) => token.issuedBy?.displayLabel ?? t('admin.system'),
      },
      {
        id: 'expires',
        label: t('admin.expires'),
        align: 'end',
        value: (token) => (token.expiresAt ? new Date(token.expiresAt).getTime() : null),
        displayText: (token) =>
          token.expiresAt
            ? formatDate(token.expiresAt, locale, { dateStyle: 'medium' })
            : t('admin.never'),
        searchText: (token) =>
          dateSearchText(
            token.expiresAt,
            token.expiresAt
              ? formatDate(token.expiresAt, locale, { dateStyle: 'medium' })
              : t('admin.never'),
          ),
      },
      {
        id: 'lastUsed',
        label: t('admin.lastUsed'),
        align: 'end',
        value: (token) => (token.lastUsedAt ? new Date(token.lastUsedAt).getTime() : null),
        displayText: (token) =>
          token.lastUsedAt
            ? formatRelative(token.lastUsedAt, locale, t('common.justNow'))
            : t('admin.never'),
        searchText: (token) =>
          dateSearchText(
            token.lastUsedAt,
            token.lastUsedAt
              ? formatRelative(token.lastUsedAt, locale, t('common.justNow'))
              : t('admin.never'),
          ),
      },
    ],
    [t, locale],
  )
}

function tokenServiceColumns(
  t: TFunction,
  onSelect: (id: string) => void,
): GridServiceColumn<APIToken>[] {
  return [
    {
      id: 'details',
      label: t('common.details'),
      cell: (token) => (
        <Button size="sm" variant="ghost" onClick={() => onSelect(token.id)}>
          {t('common.details')}
        </Button>
      ),
    },
  ]
}

export default function AccessAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const grid = useDataGridState()
  const [mintOpen, setMintOpen] = useState(false)
  const [selectedTokenId, setSelectedTokenId] = useState<string | null>(null)
  const tokensQuery = useQuery({
    ...queries.listIamTokensOptions(),
    select: (tokens: TokenResponse[]) => tokens.map((token) => mapAdminToken(token)),
  })
  const tokens = useMemo(() => tokensQuery.data ?? [], [tokensQuery.data])
  const columns = useTokenColumns()
  const serviceColumns = useMemo<GridServiceColumn<APIToken>[]>(
    () => tokenServiceColumns(t, setSelectedTokenId),
    [t],
  )

  return (
    <div className="aicfg-readable-labels">
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.accessTitle')}
        description={t('admin.accessSubtitlePolished')}
        actions={
          <Button variant="primary" onClick={() => setMintOpen(true)}>
            <Plus size={15} />
            {t('admin.mint')}
          </Button>
        }
      />
      <Panel>
        <DataGrid<APIToken>
          rows={tokens}
          rowId={tokenRowId}
          columns={columns}
          serviceColumns={serviceColumns}
          state={grid.state}
          onStateChange={grid.setState}
          ariaLabel={t('admin.accessTitle')}
          emptyText={t('admin.noTokens')}
          noMatchText={t('common.noMatches')}
          pending={tokensQuery.isPending}
          {...(tokensQuery.isError
            ? {
                errorText: errorMessage(tokensQuery.error, t('admin.loadTokensFailed')),
                onRetry: () => void tokensQuery.refetch(),
              }
            : {})}
        />
      </Panel>
      <MintTokenDialog open={mintOpen} onOpenChange={setMintOpen} />
      <Drawer
        open={selectedTokenId !== null}
        onOpenChange={(open) => {
          if (!open) setSelectedTokenId(null)
        }}
        title={tokens.find((token) => token.id === selectedTokenId)?.label ?? t('admin.tokenLabel')}
      >
        {selectedTokenId && (
          <TokenEditor tokenId={selectedTokenId} onDeleted={() => setSelectedTokenId(null)} />
        )}
      </Drawer>
    </div>
  )
}
import { getBranding } from '@/app/branding'
