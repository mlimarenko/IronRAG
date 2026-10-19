import { describe, expect, it } from 'vitest'

import type { AuditEventPageResponse } from '@/shared/api/generated'

import {
  auditColumnFilters,
  auditFacetLabel,
  auditSelectedFacets,
  compactAuditSubject,
  deriveAuditColumnSearch,
  deriveAuditSearch,
  localizeAuditAction,
  mapAuditPage,
  parseAuditFilters,
  parseAuditSort,
  serializeAuditFilters,
  shouldShowAuditMessage,
  type AuditEvent,
} from './model'

describe('mapAuditPage', () => {
  it('maps actor, internal, subject, and assistant fields', () => {
    const response: AuditEventPageResponse = {
      items: [
        {
          actionKind: 'query.execution.run',
          actorPrincipal: {
            displayLabel: 'Alex Morgan',
            id: 'actor-1',
            login: 'alex',
            principalKind: 'user',
            status: 'active',
          },
          actorPrincipalId: 'actor-1',
          assistantCall: {
            models: [{ modelName: 'atlas', providerKind: 'openai_compatible' }],
            providerCallCount: 2,
            queryExecutionId: 'query-1',
            totalCost: '0.125',
          },
          createdAt: '2026-08-09T10:00:00Z',
          id: 'audit-1',
          internalMessage: 'policy=fail_closed',
          requestId: 'request-1',
          resultKind: 'succeeded',
          subjects: [
            {
              auditEventId: 'audit-1',
              libraryId: 'library-1',
              subjectId: 'document-1',
              subjectKind: 'document',
            },
          ],
          surfaceKind: 'rest',
        },
      ],
      nextCursor: 'next',
      total: 1,
    }

    const result = mapAuditPage(response)

    expect(result.nextCursor).toBe('next')
    expect(result.items[0]).toMatchObject({
      actor: 'Alex Morgan',
      internalMessage: 'policy=fail_closed',
      libraryIds: ['library-1'],
      subjectSummary: 'document:document-1',
    })
    expect(result.items[0]?.assistantCall?.models[0]?.modelName).toBe('atlas')
  })

  it('keeps the principal id as the filter value, apart from the displayed label', () => {
    const response: AuditEventPageResponse = {
      items: [
        {
          actionKind: 'iam.session.login',
          actorPrincipalId: null,
          createdAt: '2026-08-09T10:00:00Z',
          id: 'audit-system',
          resultKind: 'succeeded',
          subjects: [],
          surfaceKind: 'worker',
        },
      ],
      total: 1,
    }

    expect(mapAuditPage(response).items[0]).toMatchObject({
      actor: '',
      actorPrincipalId: null,
    })
  })
})

describe('audit grid URL and filters', () => {
  it('round-trips typed filters and drops unknown columns and malformed keys', () => {
    const filters = { result: ['string:failed'], created_at: ['string:2026-08-09 10:00:00+00'] }
    const encoded = serializeAuditFilters(filters)
    expect(encoded).not.toBeNull()
    expect(parseAuditFilters(encoded)).toEqual(filters)
    expect(parseAuditFilters('not json')).toEqual({})
    expect(parseAuditFilters(JSON.stringify({ nope: ['string:x'], result: ['garbage'] }))).toEqual(
      {},
    )
  })

  it('maps a grid selection to typed filters, keeping the null actor a flag', () => {
    expect(
      auditColumnFilters({
        actor: ['null', 'string:actor-1'],
        result: ['string:failed'],
        surface: ['string:rest'],
      }),
    ).toEqual({
      actor: ['actor-1'],
      actorNull: true,
      resultKind: ['failed'],
      surfaceKind: ['rest'],
    })
    expect(auditColumnFilters({ actor: ['string:actor-1'] })).toEqual({ actor: ['actor-1'] })
    expect(auditColumnFilters({})).toBeUndefined()
  })

  it('parses only known audit columns and directions', () => {
    expect(parseAuditSort('actor:asc')).toEqual({ columnId: 'actor', direction: 'asc' })
    expect(parseAuditSort('created_at:desc')).toEqual({
      columnId: 'created_at',
      direction: 'desc',
    })
    expect(parseAuditSort('missing:asc')).toBeNull()
    expect(parseAuditSort('actor:sideways')).toBeNull()
    expect(parseAuditSort(null)).toBeNull()
  })

  it('uses the server label for a selected actor and never the raw id', () => {
    const translate = ((key: string) => (key === 'dataGrid.nullValue' ? '∅' : key)) as never
    const resolveLabel = (columnId: string, key: string) =>
      columnId === 'actor' && key === 'string:actor-1' ? 'Ada Lovelace' : undefined
    const selected = auditSelectedFacets(
      { actor: ['string:actor-1'] },
      translate,
      'en',
      resolveLabel,
    )
    expect(selected.actor?.[0]?.label).toBe('Ada Lovelace')
    // Without a resolved label the chip keeps the key's own text, never a
    // humanized UUID.
    const unresolved = auditSelectedFacets({ actor: ['string:actor-1'] }, translate, 'en')
    expect(unresolved.actor?.[0]?.label).toBe('actor-1')
  })

  it('labels every facet value with the same typed formatter the cells use', () => {
    const translate = ((key: string) =>
      key === 'admin.auditResultLabels.failed'
        ? 'Ошибка'
        : key === 'dataGrid.nullValue'
          ? '∅'
          : key) as never
    expect(auditFacetLabel('result', 'failed', 'failed', translate, 'en')).toBe('Ошибка')
    expect(auditFacetLabel('action', 'iam.session.login', '', translate, 'en')).toBe(
      'IAM Session Login',
    )
    expect(auditFacetLabel('actor', 'actor-1', 'Alex Morgan', translate, 'en')).toBe('Alex Morgan')
    // A null value carries the one shared null token, not an empty string.
    expect(auditFacetLabel('actor', null, 'system', translate, 'en')).toBe('∅')
  })

  it('derives the actor-null hint from the shared null token, not a keyword dictionary', () => {
    const translate = ((key: string) => (key === 'dataGrid.nullValue' ? '∅' : key)) as never
    expect(deriveAuditSearch('∅', 'en', translate)).toEqual({ actorNull: true })
    expect(deriveAuditSearch('none', 'en', translate)).toEqual({})
  })

  it('derives a facet-local hint for only the opened column', () => {
    const vocabularies: Record<string, Record<string, string>> = {
      auditResultLabels: { failed: 'Ошибка', rejected: 'Отклонено', succeeded: 'Успех' },
      auditSurfaceLabels: { rest: 'REST', mcp: 'MCP' },
    }
    const translate = ((key: string, options?: { returnObjects?: boolean }) => {
      const namespace = /^admin\.(\w+)$/.exec(key)?.[1]
      if (options?.returnObjects && namespace) return vocabularies[namespace] ?? {}
      if (key === 'admin.auditResultLabels.failed') return 'Ошибка'
      if (key === 'admin.auditSurfaceLabels.rest') return 'REST'
      if (key === 'dataGrid.nullValue') return '∅'
      return key
    }) as never
    expect(deriveAuditColumnSearch('result', 'ошиб', 'ru', translate)).toEqual({
      resultKinds: ['failed'],
    })
    // A date term resolves to its interval only on the date column.
    const date = deriveAuditColumnSearch('created_at', '02.08.2026, 10:00:00', 'ru', translate)
    expect(date.createdAtRange).toBeDefined()
    expect(deriveAuditColumnSearch('result', '02.08.2026, 10:00:00', 'ru', translate)).toEqual({})
    expect(deriveAuditColumnSearch('actor', '∅', 'ru', translate)).toEqual({ actorNull: true })
  })
})

describe('audit presentation', () => {
  const event = {
    actionKind: 'iam.session.login',
    actor: 'Alex',
    actorPrincipalId: null,
    assistantCall: undefined,
    createdAt: '2026-08-09T10:00:00Z',
    id: 'audit-1',
    internalMessage: null,
    libraryIds: [],
    redactedMessage: 'session login succeeded',
    requestId: null,
    resultKind: 'succeeded',
    subjectSummary: 'session:019fe763-64c7-7013-9f50-a09cee875cd6',
    surfaceKind: 'rest',
    traceId: null,
  } satisfies AuditEvent

  it('uses a translated action label and an acronym-safe structural fallback', () => {
    const translate = ((key: string) =>
      key === 'admin.auditActionLabels.iam.session.login' ? 'Вход в систему' : key) as never

    expect(localizeAuditAction('iam.session.login', translate)).toBe('Вход в систему')
    expect(localizeAuditAction('ai.model_catalog.update', ((key: string) => key) as never)).toBe(
      'AI Model catalog Update',
    )
  })

  it('hides redundant success copy but preserves failure details', () => {
    expect(shouldShowAuditMessage(event)).toBe(false)
    expect(
      shouldShowAuditMessage({
        ...event,
        redactedMessage: 'identity provider timeout',
        resultKind: 'failed',
      }),
    ).toBe(true)
  })

  it('compacts subject ids without losing the full source value', () => {
    expect(compactAuditSubject(event.subjectSummary)).toBe('session · 019fe763…')
    expect(compactAuditSubject('—')).toBeNull()
  })
})
