import { describe, expect, it } from 'vitest'

import {
  buildWebIngestUrlFilter,
  evaluateWebIngestUrlFilter,
  parseWebIngestPatternText,
} from '@/features/documents/model/webIngestPatterns'

describe('webIngestPatterns', () => {
  it('infers path_prefix for path-like rules', () => {
    expect(parseWebIngestPatternText('/pages/viewpage.action')).toEqual([
      { kind: 'path_prefix', value: '/pages/viewpage.action' },
    ])
  })

  it('rejects unknown typed rule prefixes instead of converting them to glob', () => {
    expect(() => parseWebIngestPatternText('regex:^/pages/')).toThrow(
      'regex is not a supported pattern kind',
    )
  })

  it('validates typed rules before the backend round trip', () => {
    expect(() => parseWebIngestPatternText('path_prefix:docs/')).toThrow(
      'path_prefix value must start with /',
    )
    expect(() => parseWebIngestPatternText('url_prefix:docs.example.com')).toThrow(
      'url_prefix value must be an absolute URL',
    )
  })

  it('deduplicates normalized rules like the backend policy parser', () => {
    expect(
      parseWebIngestPatternText(
        'path_prefix:/pages/viewpage.action\npath_prefix: /pages/viewpage.action',
      ),
    ).toEqual([{ kind: 'path_prefix', value: '/pages/viewpage.action' }])
  })

  it('matches path_prefix against the URL path', () => {
    const filter = buildWebIngestUrlFilter('path_prefix:/pages/viewpage.action', '')

    expect(
      evaluateWebIngestUrlFilter(
        'https://docs.example.com/pages/viewpage.action?pageId=123',
        filter,
      ),
    ).toMatchObject({
      passes: true,
      status: 'allowed',
      matchedPattern: {
        kind: 'path_prefix',
        value: '/pages/viewpage.action',
      },
    })
  })

  it('keeps glob semantics aligned with backend full-URL matching', () => {
    const filter = buildWebIngestUrlFilter('glob:/pages/viewpage.action*', '')

    expect(
      evaluateWebIngestUrlFilter(
        'https://docs.example.com/pages/viewpage.action?pageId=123',
        filter,
      ),
    ).toMatchObject({
      passes: false,
      status: 'no_allow_match',
    })
  })

  it('lets block patterns override allow patterns', () => {
    const filter = buildWebIngestUrlFilter(
      'path_prefix:/pages/',
      'url_prefix:https://docs.example.com/pages/archive/',
    )

    expect(
      evaluateWebIngestUrlFilter(
        'https://docs.example.com/pages/archive/viewpage.action?pageId=123',
        filter,
      ),
    ).toMatchObject({
      passes: false,
      status: 'blocked',
      matchedPattern: {
        kind: 'url_prefix',
        value: 'https://docs.example.com/pages/archive/',
      },
    })
  })
})
