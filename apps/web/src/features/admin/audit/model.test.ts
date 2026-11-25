import { renderToStaticMarkup } from 'react-dom/server'
import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import type { ReactElement } from 'react'

import type { AuditEventPageResponse, AuditEventResponse } from '@/shared/api/generated'
import type { AuditGridColumnId } from '@/shared/api'
import { gridFacetKey } from '@/shared/data-grid'
import i18n from '@/shared/i18n'

import {
  auditColumnFilters,
  auditFacetLabel,
  auditFacetOptions,
  auditGridColumns,
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
            unpricedProviderCallCount: 0,
            costCoverage: 'complete',
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

function rawEvent(overrides: Partial<AuditEventResponse> = {}): AuditEventResponse {
  return {
    actionKind: 'iam.session.login',
    createdAt: '2026-08-09T10:00:00Z',
    id: 'audit-1',
    resultKind: 'succeeded',
    subjects: [],
    surfaceKind: 'rest',
    ...overrides,
  }
}

const t = i18n.t.bind(i18n)

/** A row as the grid receives it, so the column callbacks read real fields. */
function gridEvent(overrides: Partial<AuditEvent> = {}): AuditEvent {
  return {
    actionKind: 'iam.session.login',
    actor: 'Alex Morgan',
    actorPrincipalId: 'actor-1',
    assistantCall: undefined,
    createdAt: '2026-08-09T10:00:00Z',
    id: 'audit-1',
    internalMessage: null,
    libraryIds: [],
    redactedMessage: 'session login succeeded',
    requestId: null,
    resultKind: 'succeeded',
    subjectSummary: '—',
    surfaceKind: 'rest',
    traceId: null,
    ...overrides,
  }
}

function column(id: AuditGridColumnId) {
  return auditGridColumns({ locale: 'en', showAssistant: false, showInternal: false, t }).find(
    (candidate) => candidate.id === id,
  )!
}

describe('audit actor label fallbacks', () => {
  it('falls back from the display label through name and login to the principal id', () => {
    const principal = {
      displayLabel: '   ',
      displayName: 'Ada Lovelace',
      id: 'actor-1',
      login: 'ada',
      principalKind: 'user',
      status: 'active',
    }
    const actorFor = (actorPrincipal: NonNullable<AuditEventResponse['actorPrincipal']> | null) =>
      mapAuditPage({ items: [rawEvent({ actorPrincipal })], total: 1 }).items[0]?.actor

    expect(actorFor(principal)).toBe('Ada Lovelace')
    expect(actorFor({ ...principal, displayName: '  ' })).toBe('ada')
    expect(actorFor({ ...principal, displayName: null, login: null })).toBe('actor-1')
    expect(actorFor({ ...principal, displayLabel: 'Alex Morgan' })).toBe('Alex Morgan')
    // No principal row at all reads the id the server sent, never a placeholder.
    expect(mapAuditPage({ items: [rawEvent()], total: 1 }).items[0]?.actor).toBe('')
    expect(
      mapAuditPage({ items: [rawEvent({ actorPrincipalId: 'actor-9' })], total: 1 }).items[0],
    ).toMatchObject({ actor: 'actor-9', actorPrincipalId: 'actor-9' })
  })
})

describe('audit subject mapping', () => {
  it('dedupes library ids and skips subjects that carry none', () => {
    const [item] = mapAuditPage({
      items: [
        rawEvent({
          subjects: [
            {
              auditEventId: 'audit-1',
              libraryId: 'library-1',
              subjectId: 'a',
              subjectKind: 'document',
            },
            {
              auditEventId: 'audit-1',
              libraryId: 'library-1',
              subjectId: 'b',
              subjectKind: 'chunk',
            },
            { auditEventId: 'audit-1', libraryId: null, subjectId: 'c', subjectKind: 'query' },
          ],
        }),
      ],
      total: 1,
    }).items

    expect(item?.libraryIds).toEqual(['library-1'])
    expect(item?.subjectSummary).toBe('document:a, chunk:b, query:c')
    expect(mapAuditPage({ items: [rawEvent()], total: 1 }).items[0]?.subjectSummary).toBe('—')
  })

  it('compacts a subject without a separator and an already short id', () => {
    expect(compactAuditSubject('session')).toBe('session')
    expect(compactAuditSubject('document:doc1')).toBe('document · doc1')
    expect(compactAuditSubject('query:019fe763-64c7')).toBe('query · 019fe763…')
    expect(compactAuditSubject('document:a, session:b')).toBe('document · a, session · b')
  })
})

describe('audit facet options', () => {
  it('keeps the typed key, the server count and the null bucket of a facet row', () => {
    const options = auditFacetOptions(
      'result',
      [
        { count: 2, label: 'Failed', value: 'failed' },
        { count: 1, label: '—', value: null },
      ],
      t,
      'en',
    )

    expect(options).toEqual([
      { count: 2, key: gridFacetKey('failed'), label: 'Failed', title: 'failed', value: 'failed' },
      { count: 1, key: gridFacetKey(null), label: '∅', value: null },
    ])
  })

  it('formats a timestamp facet with the same whole-timestamp format as the cell', () => {
    const label = auditFacetLabel('created_at', '2026-08-09T10:00:00Z', '', t, 'en')
    expect(label).toMatch(/^\d{2}\/\d{2}\/\d{4}, \d{2}:\d{2}:\d{2}/)
    expect(column('created_at').displayText?.(gridEvent())).toBe(label)
    // A contract value outside the closed enum keeps its raw spelling.
    expect(auditFacetLabel('surface', 'mcp', 'mcp', t, 'en')).toBe('MCP')
    expect(auditFacetLabel('nope' as AuditGridColumnId, 'raw', 'raw', t, 'en')).toBe('raw')
  })

  it('labels a selected value through its column formatter and keeps a boolean key without a numeric key', () => {
    const selected = auditSelectedFacets(
      {
        actor: ['boolean:true'],
        action: [],
        nope: ['string:x'],
        result: [gridFacetKey('failed')],
        surface: [gridFacetKey('rest')],
      },
      t,
      'en',
    )

    expect(Object.keys(selected).toSorted()).toEqual(['actor', 'result', 'surface'])
    expect(selected.actor?.[0]).toEqual({ count: -1, key: 'boolean:true', label: '∅', value: null })
    expect(selected.result?.[0]?.label).toBe('Failed')
    expect(selected.surface?.[0]?.label).toBe('REST')
  })
})

describe('audit filter decoding edge cases', () => {
  it('drops a non-object filters document and an empty column selection', () => {
    expect(parseAuditFilters(null)).toEqual({})
    expect(parseAuditFilters('')).toEqual({})
    for (const raw of ['[]', '42', 'null', '"text"', '{"result":[]}', '{"actor":"not-a-list"}']) {
      expect(parseAuditFilters(raw)).toEqual({})
    }
    expect(serializeAuditFilters({ result: [], actor: [] })).toBeNull()
  })

  it('maps every typed column, a boolean actor key and an undecodable selection', () => {
    expect(
      auditColumnFilters({
        action: ['string:query.execution.run'],
        actor: ['boolean:true'],
        created_at: ['string:2026-08-09 10:00:00+00'],
        result: [],
        surface: ['string:rest'],
      }),
    ).toEqual({
      actionKind: ['query.execution.run'],
      createdAt: ['2026-08-09 10:00:00+00'],
      surfaceKind: ['rest'],
    })
    // A key of the wrong type is not a filter value for the column.
    expect(auditColumnFilters({ action: ['number:5'] })).toBeUndefined()
    expect(auditColumnFilters({ actor: [] })).toBeUndefined()
  })

  it('ignores an empty and an unknown column search term', () => {
    expect(deriveAuditColumnSearch('created_at', '   ', 'en', t)).toEqual({})
    expect(deriveAuditColumnSearch('nope' as AuditGridColumnId, 'REST', 'en', t)).toEqual({})
    expect(deriveAuditSearch('   ', 'en', t)).toEqual({})
  })

  it('reads a whole-table term as the enum values and the interval it could have rendered', () => {
    expect(deriveAuditSearch('Failed', 'en', t)).toEqual({ resultKinds: ['failed'] })
    const range = deriveAuditSearch('08/09/2026, 10:00:00', 'en', t).createdAtRange
    expect(range?.min).toMatch(/^2026-08-09T/)
    expect(range?.max).toMatch(/^2026-08-09T/)
  })
})

describe('audit grid columns', () => {
  it('reads the facet value apart from the displayed label of every column', () => {
    const event = gridEvent({ actionKind: 'query.execution.run', resultKind: 'failed' })

    expect(column('actor').value(event)).toBe('actor-1')
    expect(column('actor').value(gridEvent({ actorPrincipalId: null }))).toBeNull()
    expect(column('action').value(event)).toBe('query.execution.run')
    expect(column('surface').value(event)).toBe('rest')
    expect(column('result').value(event)).toBe('failed')
    expect(column('created_at').value(event)).toBe('2026-08-09T10:00:00Z')
  })

  it('shows the null token for a null actor row and the actor label otherwise', () => {
    expect(column('actor').displayText?.(gridEvent({ actorPrincipalId: null }))).toBe('∅')
    expect(column('actor').displayText?.(gridEvent())).toBe('Alex Morgan')
  })

  it('shows a translated enum label in the cell and the display text of surface, action and result', () => {
    const event = gridEvent({ actionKind: 'query.execution.run', resultKind: 'failed' })

    expect(column('action').displayText?.(event)).toBe('Query Execution RUN')
    expect(column('surface').displayText?.(event)).toBe('REST')
    expect(column('result').displayText?.(event)).toBe('Failed')
    expect(renderToStaticMarkup(column('surface').cell!(event) as ReactElement)).toContain('REST')
    expect(renderToStaticMarkup(column('result').cell!(event) as ReactElement)).toContain('Failed')
    // The cell's machine-readable instant and its visible text agree with the
    // column's display text.
    const { container } = render(column('created_at').cell!(event) as ReactElement)
    expect(container.querySelector('time')?.getAttribute('datetime')).toBe(event.createdAt)
    expect(container.querySelector('time')?.textContent).toBe(
      column('created_at').displayText?.(event),
    )
  })

  it('summarizes the subject in the action cell only when there is one', () => {
    const withSubject = gridEvent({ subjectSummary: 'document:019fe763-64c7' })
    expect(renderToStaticMarkup(column('action').cell!(withSubject) as ReactElement)).toContain(
      'document · 019fe763…',
    )
    expect(renderToStaticMarkup(column('action').cell!(gridEvent()) as ReactElement)).not.toContain(
      '<small title="—"',
    )
    // A successful call whose message only repeats the action shows no copy.
    expect(renderToStaticMarkup(column('action').cell!(gridEvent()) as ReactElement)).not.toContain(
      '<p>',
    )
    expect(
      renderToStaticMarkup(
        column('action').cell!(
          gridEvent({ redactedMessage: 'provider timeout', resultKind: 'failed' }),
        ) as ReactElement,
      ),
    ).toContain('<p>provider timeout</p>')
  })

  it('shows the assistant models and cost only for an authorized assistant view', () => {
    const showAssistant = auditGridColumns({
      locale: 'en',
      showAssistant: true,
      showInternal: false,
      t,
    }).find((candidate) => candidate.id === 'action')!
    const call = {
      models: [{ modelName: 'atlas', providerKind: 'openai_compatible' }],
      providerCallCount: 2,
      queryExecutionId: 'query-1',
      totalCost: '0.125',
      currencyCode: 'USD',
      unpricedProviderCallCount: 0,
      costCoverage: 'complete' as const,
    }

    const billed = renderToStaticMarkup(
      showAssistant.cell!(gridEvent({ assistantCall: call })) as ReactElement,
    )
    expect(billed).toContain('openai_compatible:atlas')
    expect(billed).toContain('USD 0.125 · 2 completed calls')

    const partial = renderToStaticMarkup(
      showAssistant.cell!(
        gridEvent({
          assistantCall: {
            ...call,
            costCoverage: 'partial',
            unpricedProviderCallCount: 3,
          },
        }),
      ) as ReactElement,
    )
    expect(partial).toContain('USD 0.125 · partial, 3 unpriced · 2 completed calls')

    const unresolved = renderToStaticMarkup(
      showAssistant.cell!(
        gridEvent({
          assistantCall: {
            ...call,
            totalCost: null,
            currencyCode: null,
            costCoverage: 'unknown',
            unpricedProviderCallCount: 1,
            providerCallCount: 0,
          },
        }),
      ) as ReactElement,
    )
    expect(unresolved).toContain('Cost unknown · 1 unpriced · 0 completed calls')

    const unbilled = renderToStaticMarkup(
      showAssistant.cell!(
        gridEvent({
          assistantCall: { ...call, models: [], costCoverage: null },
        }),
      ) as ReactElement,
    )
    expect(unbilled).toContain('No billed model')
    expect(unbilled).toContain('Cost unavailable')
    expect(unbilled).not.toContain('USD 0.125')

    // The same row outside the assistant view carries no call metadata.
    expect(
      renderToStaticMarkup(
        column('action').cell!(gridEvent({ assistantCall: call })) as ReactElement,
      ),
    ).not.toContain('atlas')
  })

  it('adds request, trace and internal detail only for an authorized internal view', () => {
    const internal = auditGridColumns({
      locale: 'en',
      showAssistant: false,
      showInternal: true,
      t,
    }).find((candidate) => candidate.id === 'action')!
    const event = gridEvent({
      internalMessage: 'policy=fail_closed',
      requestId: 'request-1',
      traceId: 'trace-1',
    })

    const markup = renderToStaticMarkup(internal.cell!(event) as ReactElement)
    expect(markup).toContain('request_id=request-1')
    expect(markup).toContain('trace_id=trace-1')
    expect(markup).toContain('policy=fail_closed')

    const blank = renderToStaticMarkup(internal.cell!(gridEvent()) as ReactElement)
    expect(blank).toContain('request_id=— · trace_id=—')
    expect(blank).not.toContain('policy=')
    expect(renderToStaticMarkup(column('action').cell!(event) as ReactElement)).not.toContain(
      'request_id=',
    )
  })
})
