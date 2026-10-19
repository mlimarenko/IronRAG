import { useMemo, useState } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  Building2,
  ExternalLink,
  LoaderCircle,
  Plus,
  RefreshCw,
  SearchX,
  Trash2,
  Webhook as WebhookIcon,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { queries } from '@/shared/api'
import type {
  CreateSubscriptionRequest,
  SubscriptionResponse,
  UpdateSubscriptionRequest,
} from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { formatRelative } from '@/shared/format'
import { deliveryStateLabel, webhookEventLabel } from '@/shared/lib/enumLabels'
import { errorMessage } from '@/shared/lib/errorMessage'
import { screenPhase } from '@/shared/lib/screenPhase'
import { Badge, Button, Drawer, ErrorState, Modal, PageHeader, Panel, SearchBox } from '@/shared/ui'
import { useWebhookMutations } from './api/webhooks'
import {
  DELIVERY_STATES,
  WEBHOOK_EVENT_TYPES,
  customHeadersBody,
  emptyWebhookForm,
  deliveryTone,
  mapWebhookDelivery,
  validateWebhookForm,
  webhookListState,
  webhookStatus,
  WEBHOOK_STATUS_TONE,
  type WebhookStatus,
  type DeliveryState,
  type WebhookEventType,
  type WebhookForm,
} from './model/webhookModel'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

// i18n keys for the three webhook statuses, kept next to the tone map so a new
// status cannot gain a colour without gaining a label.
const WEBHOOK_STATUS_LABEL_KEY: Readonly<Record<WebhookStatus, string>> = {
  active: 'common.active',
  draining: 'admin.draining',
  paused: 'common.paused',
}

// A React list key for a header row the operator just added. `useId` cannot
// help here — the row is created inside an event handler, not during render —
// and `Math.random` is flagged as a weak generator wherever it appears, which
// is noise on a DOM id and a real finding everywhere else. `randomUUID` costs
// nothing here and keeps the warning meaningful when it fires elsewhere.
function headerId(): string {
  return `header-${crypto.randomUUID()}`
}

function WebhookEditor({
  onClose,
  subscriptionId,
  workspaceId,
}: Readonly<{ onClose: () => void; subscriptionId: string | null; workspaceId: string }>) {
  const { t } = useTranslation()
  const { createWebhook, updateWebhook } = useWebhookMutations(workspaceId)
  const [formDraft, setFormDraft] = useState<WebhookForm | null>(null)
  const detailQuery = useQuery({
    ...queries.getWebhookSubscriptionOptions({ path: { subscriptionId: subscriptionId ?? '' } }),
    enabled: subscriptionId !== null,
  })
  const librariesQuery = useQuery({
    ...queries.listCatalogLibrariesOptions({
      path: { workspaceId },
      query: { limit: 200, offset: 0 },
    }),
    enabled: subscriptionId === null,
  })
  const editing = subscriptionId !== null
  const detailPhase = screenPhase({
    loading: editing && detailQuery.isLoading,
    failed: editing && detailQuery.isError,
  })
  const initialForm = useMemo<WebhookForm>(() => {
    const subscription = detailQuery.data
    if (!subscription) return emptyWebhookForm()
    return {
      ...emptyWebhookForm(),
      active: subscription.active,
      displayName: subscription.displayName,
      eventTypes: subscription.eventTypes.filter((event): event is WebhookEventType =>
        WEBHOOK_EVENT_TYPES.includes(event as WebhookEventType),
      ),
      libraryId: subscription.libraryId ?? '',
      scope: subscription.libraryId ? 'library' : 'workspace',
      targetUrl: subscription.targetUrl,
    }
  }, [detailQuery.data])
  const form = formDraft ?? initialForm
  const updateForm = (update: (current: WebhookForm) => WebhookForm) => {
    setFormDraft((current) => update(current ?? initialForm))
  }
  const pending = createWebhook.isPending || updateWebhook.isPending
  const errors = validateWebhookForm(form, { requireSecret: !editing })
  const valid = Object.keys(errors).length === 0

  const save = () => {
    if (!valid) return
    const headers = customHeadersBody(form.customHeaders)
    if (editing && subscriptionId) {
      const body: UpdateSubscriptionRequest = {
        active: form.active,
        displayName: form.displayName.trim(),
        eventTypes: form.eventTypes,
        targetUrl: form.targetUrl,
        ...(form.secret ? { secret: form.secret } : {}),
        ...(headers ? { customHeaders: headers } : {}),
      }
      updateWebhook.mutate(
        { workspaceId, subscriptionId, body },
        {
          onSuccess: () => {
            toast.success(t('admin.webhooks.updated'))
            onClose()
          },
          onError: (error) => toast.error(errorMessage(error, t('admin.webhooks.updateFailed'))),
        },
      )
      return
    }
    const body: CreateSubscriptionRequest = {
      displayName: form.displayName.trim(),
      targetUrl: form.targetUrl,
      eventTypes: form.eventTypes,
      secret: form.secret,
      ...(form.scope === 'library' ? { libraryId: form.libraryId } : {}),
      ...(headers ? { customHeaders: headers } : {}),
    }
    createWebhook.mutate(
      { workspaceId, body },
      {
        onSuccess: () => {
          toast.success(t('admin.webhooks.created'))
          onClose()
        },
        onError: (error) => toast.error(errorMessage(error, t('admin.webhooks.createFailed'))),
      },
    )
  }

  const toggleEvent = (eventType: WebhookEventType, checked: boolean) => {
    updateForm((current) => ({
      ...current,
      eventTypes: checked
        ? [...current.eventTypes, eventType]
        : current.eventTypes.filter((event) => event !== eventType),
    }))
  }

  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      title={editing ? t('common.edit') : t('common.create')}
      description={t('admin.webhooksSubtitle')}
      wide
      footer={
        <>
          <Button onClick={onClose}>{t('common.cancel')}</Button>
          <Button
            variant="primary"
            disabled={!valid || pending || (editing && detailQuery.isLoading)}
            onClick={save}
          >
            {pending ? t('admin.saving') : t('common.save')}
          </Button>
        </>
      }
    >
      {detailPhase === 'loading' && (
        <div className="state-card aicfg-loading-state">
          <LoaderCircle className="spin" size={22} />
          <strong>{t('common.loading')}</strong>
        </div>
      )}
      {detailPhase === 'error' && (
        <div className="state-card state-card--error">
          <AlertTriangle size={22} />
          <div>
            <strong>{t('admin.webhooks.detailFailed')}</strong>
            <Button size="sm" onClick={() => void detailQuery.refetch()}>
              {t('common.tryAgain')}
            </Button>
          </div>
        </div>
      )}
      {detailPhase === 'ready' && (
        <div className="field-grid">
          <label className="field">
            <span>{t('common.name')}</span>
            <input
              className="input"
              value={form.displayName}
              maxLength={128}
              onChange={(event) =>
                updateForm((current) => ({ ...current, displayName: event.target.value }))
              }
            />
            {errors.displayName && (
              <small className="field-error">{t('admin.webhooks.nameInvalid')}</small>
            )}
          </label>
          <label className="field">
            <span>{t('admin.targetUrl')}</span>
            <input
              className="input"
              type="url"
              value={form.targetUrl}
              onChange={(event) =>
                updateForm((current) => ({ ...current, targetUrl: event.target.value }))
              }
            />
            {errors.targetUrl && (
              <small className="field-error">{t('admin.webhooks.urlInvalid')}</small>
            )}
          </label>
          <label className="field">
            <span>{t('common.scope')}</span>
            <select
              className="input"
              disabled={editing}
              value={form.scope}
              onChange={(event) =>
                updateForm((current) => ({
                  ...current,
                  scope: event.target.value as WebhookForm['scope'],
                  libraryId: '',
                }))
              }
            >
              <option value="workspace">{t('admin.webhooks.entireWorkspace')}</option>
              <option value="library">{t('common.library')}</option>
            </select>
          </label>
          {form.scope === 'library' && (
            <label className="field">
              <span>{t('common.library')}</span>
              <select
                className="input"
                disabled={editing}
                value={form.libraryId}
                onChange={(event) =>
                  updateForm((current) => ({ ...current, libraryId: event.target.value }))
                }
              >
                <option value="">{t('admin.webhooks.selectLibrary')}</option>
                {(librariesQuery.data?.items ?? []).map((library) => (
                  <option key={library.id} value={library.id}>
                    {library.displayName}
                  </option>
                ))}
              </select>
              {errors.libraryId && (
                <small className="field-error">{t('admin.webhooks.libraryRequired')}</small>
              )}
              {librariesQuery.data &&
                librariesQuery.data.totalCount > librariesQuery.data.items.length && (
                  <small className="aicfg-library-limit-warning">
                    {t('admin.libraryPicker.partial', {
                      shown: librariesQuery.data.items.length,
                      total: librariesQuery.data.totalCount,
                    })}
                  </small>
                )}
            </label>
          )}
          {editing && (
            <label className="field">
              <span>{t('common.status')}</span>
              <select
                className="input"
                value={form.active ? 'active' : 'paused'}
                onChange={(event) =>
                  updateForm((current) => ({ ...current, active: event.target.value === 'active' }))
                }
              >
                <option value="active">{t('common.active')}</option>
                <option value="paused">{t('common.paused')}</option>
              </select>
            </label>
          )}
          <div className="field field--wide">
            <span>{t('admin.eventTypes')}</span>
            <div className="checkbox-row">
              {WEBHOOK_EVENT_TYPES.map((eventType) => (
                <label key={eventType}>
                  <input
                    type="checkbox"
                    checked={form.eventTypes.includes(eventType)}
                    onChange={(event) => toggleEvent(eventType, event.target.checked)}
                  />
                  {webhookEventLabel(eventType, t)}
                </label>
              ))}
            </div>
            {errors.eventTypes && (
              <small className="field-error">{t('admin.webhooks.eventRequired')}</small>
            )}
          </div>
          <label className="field field--wide">
            <span>{t('admin.secret')}</span>
            <input
              className="input"
              type="password"
              value={form.secret}
              placeholder={editing ? t('admin.writeOnly') : t('admin.webhooks.secretPlaceholder')}
              onChange={(event) =>
                updateForm((current) => ({ ...current, secret: event.target.value }))
              }
            />
            {errors.secret && (
              <small className="field-error">{t('admin.webhooks.secretInvalid')}</small>
            )}
          </label>
          <div className="field field--wide">
            <span>{t('admin.customHeaders')}</span>
            {form.customHeaders.map((header) => (
              <div className="header-editor" key={header.id}>
                <input
                  aria-label={t('admin.webhooks.headerName')}
                  className="input"
                  value={header.name}
                  onChange={(event) =>
                    updateForm((current) => ({
                      ...current,
                      customHeaders: current.customHeaders.map((candidate) =>
                        candidate.id === header.id
                          ? { ...candidate, name: event.target.value }
                          : candidate,
                      ),
                    }))
                  }
                />
                <input
                  aria-label={t('admin.webhooks.headerValue')}
                  className="input"
                  value={header.value}
                  onChange={(event) =>
                    updateForm((current) => ({
                      ...current,
                      customHeaders: current.customHeaders.map((candidate) =>
                        candidate.id === header.id
                          ? { ...candidate, value: event.target.value }
                          : candidate,
                      ),
                    }))
                  }
                />
                <Button
                  size="sm"
                  onClick={() =>
                    updateForm((current) => ({
                      ...current,
                      customHeaders: current.customHeaders.filter(
                        (candidate) => candidate.id !== header.id,
                      ),
                    }))
                  }
                >
                  {t('common.delete')}
                </Button>
              </div>
            ))}
            <Button
              size="sm"
              disabled={form.customHeaders.length >= 32}
              onClick={() =>
                updateForm((current) => ({
                  ...current,
                  customHeaders: [
                    ...current.customHeaders,
                    { id: headerId(), name: '', value: '' },
                  ],
                }))
              }
            >
              <Plus size={13} />
              {t('admin.add')}
            </Button>
            {errors.customHeaders && (
              <small className="field-error">{t('admin.webhooks.headersInvalid')}</small>
            )}
            <small>{t('admin.webhooks.writeOnlyHeaders')}</small>
          </div>
        </div>
      )}
    </Modal>
  )
}

function DeliveryDrawer({
  onClose,
  subscription,
}: Readonly<{ onClose: () => void; subscription: SubscriptionResponse }>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const [state, setState] = useState<DeliveryState | 'all'>('all')
  const attemptsQuery = useQuery({
    ...queries.listWebhookDeliveryAttemptsOptions({
      path: { subscriptionId: subscription.id },
      query: { limit: 200, ...(state === 'all' ? {} : { state }) },
    }),
    placeholderData: keepPreviousData,
    select: (page) => ({ ...page, items: page.items.map(mapWebhookDelivery) }),
  })
  const attempts = attemptsQuery.data?.items ?? []
  const deliveriesPhase = screenPhase({
    loading: attemptsQuery.isLoading,
    failed: attemptsQuery.isError,
    empty: attempts.length === 0,
  })

  return (
    <Drawer
      open
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      title={`${t('admin.deliveries')} · ${subscription.displayName}`}
    >
      <div className="toolbar">
        <select
          className="input"
          value={state}
          onChange={(event) => setState(event.target.value as DeliveryState | 'all')}
        >
          <option value="all">{t('common.all')}</option>
          {DELIVERY_STATES.map((deliveryState) => (
            <option key={deliveryState} value={deliveryState}>
              {deliveryStateLabel(deliveryState, t)}
            </option>
          ))}
        </select>
        <Button
          size="icon"
          aria-label={t('common.refresh')}
          onClick={() => void attemptsQuery.refetch()}
        >
          <RefreshCw size={14} />
        </Button>
      </div>
      {deliveriesPhase === 'loading' && (
        <div className="state-card aicfg-loading-state">
          <LoaderCircle className="spin" size={22} />
          <strong>{t('common.loading')}</strong>
        </div>
      )}
      {deliveriesPhase === 'error' && (
        <div className="state-card state-card--error">
          <AlertTriangle size={22} />
          <strong>{t('admin.webhooks.deliveriesFailed')}</strong>
        </div>
      )}
      {deliveriesPhase === 'empty' && (
        <div className="state-card state-card--stacked aicfg-empty-state">
          <SearchX size={26} />
          <div>
            <strong>{t('admin.webhooks.noDeliveries')}</strong>
          </div>
        </div>
      )}
      {deliveriesPhase === 'ready' && (
        <div className="delivery-list">
          {attempts.map((delivery) => (
            <article key={delivery.id}>
              <div>
                <strong>{webhookEventLabel(delivery.eventType, t)}</strong>
                <small>{t('admin.webhooks.attempt', { number: delivery.attemptNumber })}</small>
              </div>
              <Badge tone={deliveryTone(delivery.deliveryState)}>
                {deliveryStateLabel(delivery.deliveryState, t)}
              </Badge>
              <span>{delivery.responseStatus ? `HTTP ${delivery.responseStatus}` : '—'}</span>
              <time>{formatRelative(delivery.createdAt, locale, t('common.justNow'))}</time>
              {delivery.errorMessage && (
                <p>
                  {delivery.errorMessage}
                  {delivery.nextAttemptAt
                    ? ` · ${t('admin.webhooks.nextRetry', { time: formatRelative(delivery.nextAttemptAt, locale, t('common.justNow')) })}`
                    : ''}
                </p>
              )}
            </article>
          ))}
        </div>
      )}
    </Drawer>
  )
}

export default function WebhooksAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const { activeWorkspace, locale } = useApp()
  const workspaceId = activeWorkspace?.id ?? ''
  const { deleteWebhook } = useWebhookMutations(workspaceId)
  // Either a subscription id or the literal 'new'; both are strings, so the
  // union collapsed and the discriminant lived only in the reader's head.
  const [editor, setEditor] = useState<string | null>(null)
  const [deliveries, setDeliveries] = useState<SubscriptionResponse | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<SubscriptionResponse | null>(null)
  const [drainingIds, setDrainingIds] = useState<Set<string>>(() => new Set())
  const [search, setSearch] = useState('')
  const [statusFilter, setStatusFilter] = useState<'all' | 'active' | 'paused' | 'draining'>('all')
  const subscriptionsQuery = useQuery({
    ...queries.listWebhookSubscriptionsOptions({ path: { workspaceId }, query: { limit: 200 } }),
    enabled: Boolean(workspaceId),
    placeholderData: keepPreviousData,
  })
  const subscriptions = useMemo(
    () => subscriptionsQuery.data?.items ?? [],
    [subscriptionsQuery.data],
  )
  const filtered = useMemo(
    () =>
      subscriptions.filter((subscription) => {
        const draining = drainingIds.has(subscription.id)
        const status = webhookStatus(draining, subscription.active)
        return (
          (statusFilter === 'all' || status === statusFilter) &&
          subscription.displayName.toLowerCase().includes(search.trim().toLowerCase())
        )
      }),
    [drainingIds, search, statusFilter, subscriptions],
  )
  const listPhase = screenPhase({
    loading: subscriptionsQuery.isLoading,
    failed: subscriptionsQuery.isError,
  })
  const listState = webhookListState(subscriptions.length, filtered.length)

  const remove = () => {
    if (!deleteTarget) return
    const target = deleteTarget
    deleteWebhook.mutate(
      {
        workspaceId,
        subscriptionId: target.id,
        onDraining: () => {
          setDrainingIds((current) => new Set(current).add(target.id))
          setDeleteTarget(null)
          toast.warning(t('admin.draining'))
        },
      },
      {
        onSuccess: () => {
          setDrainingIds((current) => {
            const next = new Set(current)
            next.delete(target.id)
            return next
          })
          setDeleteTarget(null)
          toast.success(t('admin.webhooks.deleted'))
        },
        onError: (error) => toast.error(errorMessage(error, t('admin.webhooks.deleteFailed'))),
      },
    )
  }

  if (!workspaceId) {
    return (
      <div>
        <PageHeader
          eyebrow={adminBreadcrumb}
          title={t('admin.webhooksTitle')}
          description={t('admin.webhooksSubtitle')}
        />
        <div className="state-card state-card--stacked">
          <Building2 size={26} />
          <div>
            <strong>{t('admin.webhooks.workspaceRequired')}</strong>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="aicfg-readable-labels">
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.webhooksTitle')}
        description={t('admin.webhooksSubtitle')}
        actions={
          <Button variant="primary" onClick={() => setEditor('new')}>
            <Plus size={15} />
            {t('common.create')}
          </Button>
        }
      />
      {listPhase === 'loading' && (
        <div className="state-card aicfg-loading-state">
          <LoaderCircle className="spin" size={22} />
          <strong>{t('common.loading')}</strong>
        </div>
      )}
      {listPhase === 'error' && (
        <ErrorState
          title={t('admin.webhooks.loadFailed')}
          onRetry={() => void subscriptionsQuery.refetch()}
        />
      )}
      {listPhase === 'ready' && (
        <>
          <div className="toolbar">
            <SearchBox value={search} onChange={setSearch} />
            <select
              className="input compact-input"
              value={statusFilter}
              onChange={(event) => setStatusFilter(event.target.value as typeof statusFilter)}
            >
              <option value="all">{t('common.all')}</option>
              <option value="active">{t('common.active')}</option>
              <option value="paused">{t('common.paused')}</option>
              <option value="draining">{t('admin.draining')}</option>
            </select>
          </div>
          {listState === 'no-webhooks' && (
            <div className="state-card state-card--stacked aicfg-empty-state">
              <WebhookIcon size={28} />
              <div>
                <strong>{t('admin.webhooks.emptyTitle')}</strong>
                <p>{t('admin.webhooks.emptyDescription')}</p>
                <Button variant="primary" onClick={() => setEditor('new')}>
                  <Plus size={14} />
                  {t('admin.webhooks.createAction')}
                </Button>
              </div>
            </div>
          )}
          {listState === 'no-matches' && (
            <div className="state-card state-card--stacked aicfg-empty-state">
              <SearchX size={26} />
              <div>
                <strong>{t('admin.webhooks.noMatches')}</strong>
                <p>{t('admin.webhooks.noMatchesDescription')}</p>
              </div>
            </div>
          )}
          {listState === 'ready' && (
            <div className="webhook-grid">
              {filtered.map((hook) => {
                const draining = drainingIds.has(hook.id)
                const status = webhookStatus(draining, hook.active)
                return (
                  <Panel className="webhook-card" key={hook.id}>
                    <header>
                      <span>
                        <WebhookIcon size={18} />
                      </span>
                      <div>
                        <strong>{hook.displayName}</strong>
                        <a href={hook.targetUrl} target="_blank" rel="noreferrer">
                          {hook.targetUrl}
                          <ExternalLink size={11} />
                        </a>
                      </div>
                      <Badge tone={WEBHOOK_STATUS_TONE[status]}>
                        {t(WEBHOOK_STATUS_LABEL_KEY[status])}
                      </Badge>
                    </header>
                    <div className="webhook-events">
                      {hook.eventTypes.map((event) => (
                        <Badge key={event}>{webhookEventLabel(event, t)}</Badge>
                      ))}
                    </div>
                    <dl>
                      <div>
                        <dt>{t('common.scope')}</dt>
                        <dd>{hook.libraryId ? t('common.library') : activeWorkspace?.name}</dd>
                      </div>
                      <div>
                        <dt>{t('common.created')}</dt>
                        <dd>{formatRelative(hook.createdAt, locale, t('common.justNow'))}</dd>
                      </div>
                    </dl>
                    <footer>
                      <Button size="sm" onClick={() => setDeliveries(hook)}>
                        {t('admin.deliveries')}
                      </Button>
                      <Button size="sm" disabled={draining} onClick={() => setEditor(hook.id)}>
                        {t('common.edit')}
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        disabled={draining || deleteWebhook.isPending}
                        aria-label={t('common.delete')}
                        onClick={() => setDeleteTarget(hook)}
                      >
                        <Trash2 size={15} />
                      </Button>
                    </footer>
                  </Panel>
                )
              })}
            </div>
          )}
        </>
      )}
      {editor && (
        <WebhookEditor
          workspaceId={workspaceId}
          subscriptionId={editor === 'new' ? null : editor}
          onClose={() => setEditor(null)}
        />
      )}
      {deliveries && (
        <DeliveryDrawer subscription={deliveries} onClose={() => setDeliveries(null)} />
      )}
      <Modal
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        title={t('admin.webhooks.deleteTitle')}
        description={t('admin.webhooks.deleteDescription', {
          name: deleteTarget?.displayName ?? '',
        })}
        footer={
          <>
            <Button onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
            <Button variant="danger" disabled={deleteWebhook.isPending} onClick={remove}>
              {t('common.delete')}
            </Button>
          </>
        }
      >
        <div className="inline-alert">
          <AlertTriangle size={15} />
          {t('admin.webhooks.deleteDrainingNote')}
        </div>
      </Modal>
    </div>
  )
}
