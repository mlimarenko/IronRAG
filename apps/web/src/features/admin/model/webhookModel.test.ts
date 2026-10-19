import { describe, expect, it } from 'vitest'
import {
  customHeadersBody,
  deliveryTone,
  emptyWebhookForm,
  validateWebhookForm,
  webhookListState,
  webhookStatus,
  WEBHOOK_STATUS_TONE,
  type WebhookForm,
} from './webhookModel'

describe('webhookModel', () => {
  it('requires an HTTP target, events, and a 32-byte create secret', () => {
    const errors = validateWebhookForm(
      {
        ...emptyWebhookForm(),
        displayName: 'Events',
        targetUrl: 'ftp://example.test',
        secret: 'short',
      },
      { requireSecret: true },
    )
    expect(errors).toMatchObject({
      eventTypes: 'eventTypes',
      secret: 'secret',
      targetUrl: 'targetUrl',
    })
  })

  it('accepts a valid workspace subscription and serializes headers', () => {
    const form: WebhookForm = {
      ...emptyWebhookForm(),
      displayName: 'Lifecycle events',
      targetUrl: 'https://automation.example.test/hooks',
      secret: '12345678901234567890123456789012',
      eventTypes: ['revision.ready'],
      customHeaders: [{ id: 'header-1', name: 'X-Environment', value: 'test' }],
    }
    expect(validateWebhookForm(form, { requireSecret: true })).toEqual({})
    expect(customHeadersBody(form.customHeaders)).toEqual({ 'X-Environment': 'test' })
  })
})

describe('webhookStatus', () => {
  it('ranks draining above the stored active flag', () => {
    expect(webhookStatus(true, true)).toBe('draining')
    expect(webhookStatus(true, false)).toBe('draining')
  })

  it('separates active from paused when nothing is draining', () => {
    expect(webhookStatus(false, true)).toBe('active')
    expect(webhookStatus(false, false)).toBe('paused')
  })

  it('has a tone for every status', () => {
    expect(Object.keys(WEBHOOK_STATUS_TONE).sort()).toEqual(['active', 'draining', 'paused'])
  })
})

describe('webhookListState', () => {
  it('distinguishes "nothing exists" from "the filter excluded everything"', () => {
    expect(webhookListState(0, 0)).toBe('no-webhooks')
    expect(webhookListState(4, 0)).toBe('no-matches')
    expect(webhookListState(4, 2)).toBe('ready')
  })
})

describe('deliveryTone', () => {
  it('treats abandoned as a failure and pre-terminal states as in flight', () => {
    expect(deliveryTone('delivered')).toBe('ready')
    expect(deliveryTone('failed')).toBe('failed')
    expect(deliveryTone('abandoned')).toBe('failed')
    expect(deliveryTone('pending')).toBe('processing')
    expect(deliveryTone('delivering')).toBe('processing')
  })
})
