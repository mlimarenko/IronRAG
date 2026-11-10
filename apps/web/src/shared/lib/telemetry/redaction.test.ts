import { describe, expect, it } from 'vitest'

import { normalizeRoute, redactText, sanitizeAttributes, sanitizeAttributeValue } from './redaction'

describe('telemetry redaction', () => {
  it('removes personal identifiers and credentials from free text', () => {
    const text = redactText(
      'failed for alice@example.com with Bearer abcdefghijklmnop and +7 (999) 123-45-67',
    )
    expect(text).not.toContain('alice@example.com')
    expect(text).not.toContain('abcdefghijklmnop')
    expect(text).not.toContain('+7 (999) 123-45-67')
    expect(text).toContain('[redacted-email]')
    expect(text).toContain('[redacted-credential]')
    expect(text).toContain('[redacted-phone]')
  })

  it('keeps a URL path but drops its query string and secret parameters', () => {
    const text = redactText('GET https://graph.piping.space/assets/app.js?token=secret#frag failed')
    expect(text).toContain('https://graph.piping.space/assets/app.js')
    expect(text).not.toContain('token=secret')
    expect(text).not.toContain('#frag')
  })

  it('normalises generated identifiers out of routes', () => {
    expect(normalizeRoute('/v1/query/sessions/550e8400-e29b-41d4-a716-446655440000/turns')).toBe(
      '/v1/query/sessions/:id/turns',
    )
    expect(normalizeRoute('/documents/42?filter=secret')).toBe('/documents/:id')
    expect(normalizeRoute(null)).toBe('unknown')
  })

  it('drops forbidden attribute names and non-primitive values', () => {
    const attributes = sanitizeAttributes({
      'page.route': '/assistant',
      'session.id': 'session-1',
      'user.email': 'alice@example.com',
      email: 'alice@example.com',
      authorization: 'Bearer secret',
      nested: { unsafe: true },
      'empty.value': '',
      duration_ms: 12,
    })
    expect(attributes).toEqual({
      'page.route': '/assistant',
      'session.id': 'session-1',
      duration_ms: 12,
    })
  })

  it('redacts and truncates individual values', () => {
    expect(sanitizeAttributeValue('alice@example.com')).toBe('[redacted-email]')
    expect(sanitizeAttributeValue('x'.repeat(500))?.toString().length).toBeLessThanOrEqual(257)
    expect(sanitizeAttributeValue(Number.NaN)).toBeUndefined()
  })
})
