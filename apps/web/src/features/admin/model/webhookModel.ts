import type { DeliveryAttemptResponse } from '@/shared/api/generated'

export const WEBHOOK_EVENT_TYPES = ['revision.ready', 'document.deleted'] as const
export const DELIVERY_STATES = [
  'pending',
  'delivering',
  'delivered',
  'failed',
  'abandoned',
] as const

export type WebhookEventType = (typeof WEBHOOK_EVENT_TYPES)[number]
export type DeliveryState = (typeof DELIVERY_STATES)[number]
export type CustomHeader = Readonly<{ id: string; name: string; value: string }>

export type WebhookForm = {
  active: boolean
  customHeaders: CustomHeader[]
  displayName: string
  eventTypes: WebhookEventType[]
  libraryId: string
  scope: 'workspace' | 'library'
  secret: string
  targetUrl: string
}

export type WebhookFormErrors = Partial<
  Record<
    'customHeaders' | 'displayName' | 'eventTypes' | 'libraryId' | 'secret' | 'targetUrl',
    string
  >
>

export function emptyWebhookForm(): WebhookForm {
  return {
    active: true,
    customHeaders: [],
    displayName: '',
    eventTypes: [],
    libraryId: '',
    scope: 'workspace',
    secret: '',
    targetUrl: '',
  }
}

export function validateWebhookForm(
  form: WebhookForm,
  options: Readonly<{ requireSecret: boolean }>,
): WebhookFormErrors {
  const errors: WebhookFormErrors = {}
  const displayNameLength = form.displayName.trim().length
  if (displayNameLength < 1 || displayNameLength > 128) errors.displayName = 'displayName'

  try {
    const url = new URL(form.targetUrl)
    if (url.protocol !== 'http:' && url.protocol !== 'https:') errors.targetUrl = 'targetUrl'
  } catch {
    errors.targetUrl = 'targetUrl'
  }

  if (form.eventTypes.length === 0) errors.eventTypes = 'eventTypes'
  if (form.scope === 'library' && !form.libraryId) errors.libraryId = 'libraryId'
  if (options.requireSecret && new TextEncoder().encode(form.secret).length < 32) {
    errors.secret = 'secret'
  }
  if (form.secret && new TextEncoder().encode(form.secret).length < 32) errors.secret = 'secret'
  if (form.customHeaders.length > 32) errors.customHeaders = 'customHeaders'
  if (form.customHeaders.some((header) => !header.name.trim() || !header.value.trim())) {
    errors.customHeaders = 'customHeaders'
  }
  return errors
}

export function customHeadersBody(headers: CustomHeader[]): Record<string, string> | undefined {
  if (headers.length === 0) return undefined
  return Object.fromEntries(headers.map((header) => [header.name.trim(), header.value.trim()]))
}

function deliveryState(value: string): DeliveryState {
  const state = DELIVERY_STATES.find((candidate) => candidate === value)
  if (!state) throw new Error(`Webhook delivery has invalid state: ${value}`)
  return state
}

export type WebhookDelivery = Omit<DeliveryAttemptResponse, 'deliveryState'> & {
  deliveryState: DeliveryState
}

export function mapWebhookDelivery(raw: DeliveryAttemptResponse): WebhookDelivery {
  return { ...raw, deliveryState: deliveryState(raw.deliveryState) }
}

/**
 * What a subscription is doing right now. `draining` is not a stored flag: a
 * subscription being drained is still `active` in the API, so the three states
 * only exist once the in-flight drain set is folded in. The status filter, the
 * badge tone and the badge label all used to re-derive it inline and could
 * therefore disagree with each other.
 */
export type WebhookStatus = 'draining' | 'active' | 'paused'

export function webhookStatus(draining: boolean, active: boolean): WebhookStatus {
  if (draining) return 'draining'
  return active ? 'active' : 'paused'
}

export const WEBHOOK_STATUS_TONE: Readonly<Record<WebhookStatus, string>> = {
  active: 'active',
  draining: 'warning',
  paused: 'paused',
}

/**
 * Why a webhook list is showing no cards: nothing exists yet (offer to create
 * one) or the filters excluded everything (offer to clear them). Answering
 * both with one empty state sent operators to the create dialog when all they
 * had done was type into the search box.
 */
export type WebhookListState = 'no-webhooks' | 'no-matches' | 'ready'

export function webhookListState(total: number, matching: number): WebhookListState {
  if (total === 0) return 'no-webhooks'
  if (matching === 0) return 'no-matches'
  return 'ready'
}

/**
 * Badge tone for a delivery attempt. `abandoned` reads as a failure because
 * the delivery is never retried again; everything before a terminal state is
 * still in flight.
 */
export function deliveryTone(state: DeliveryState): string {
  if (state === 'delivered') return 'ready'
  if (state === 'failed' || state === 'abandoned') return 'failed'
  return 'processing'
}
