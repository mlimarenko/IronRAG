import { describe, expect, it } from 'vitest'

import i18n from '@/shared/i18n'
import { localizeAttention, resolveAttentionRoute } from './format'
import type { DashboardAttentionItem } from './types'

function attention(overrides: Partial<DashboardAttentionItem> = {}): DashboardAttentionItem {
  return {
    code: 'failed_documents',
    title: 'backend title',
    detail: 'backend detail',
    routePath: '/documents?status=failed',
    level: 'error',
    ...overrides,
  }
}

describe('dashboard attention formatting', () => {
  it('uses backend-owned internal route paths for attention navigation', () => {
    expect(resolveAttentionRoute(attention())).toBe('/documents?status=failed')
    expect(resolveAttentionRoute(attention({ routePath: '/graph' }))).toBe('/graph')
  })

  it('rejects empty and external attention route paths', () => {
    expect(resolveAttentionRoute(attention({ routePath: '' }))).toBe('/dashboard')
    expect(resolveAttentionRoute(attention({ routePath: '//example.test/path' }))).toBe(
      '/dashboard',
    )
    expect(resolveAttentionRoute(attention({ routePath: 'https://example.test/path' }))).toBe(
      '/dashboard',
    )
  })

  it('localizes known attention codes and supplies a visible action label', () => {
    const content = localizeAttention(attention(), i18n.t.bind(i18n))

    expect(content.title).toBe('Failed documents need review')
    expect(content.detail).toBe(
      'Some documents are currently failed in the active library and need operator review.',
    )
    expect(content.action).toBe('Review failed documents')
  })

  it('keeps backend copy for unknown attention codes and still labels the action', () => {
    const content = localizeAttention(
      attention({ code: 'unknown_signal', title: 'Backend title', detail: 'Backend detail' }),
      i18n.t.bind(i18n),
    )

    expect(content).toEqual({
      title: 'Backend title',
      detail: 'Backend detail',
      action: 'Open related view',
    })
  })
})
