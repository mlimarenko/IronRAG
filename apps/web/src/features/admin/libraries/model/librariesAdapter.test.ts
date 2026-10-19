import type { AdminLibraryPageResponse, CatalogLibraryResponse } from '@/shared/api/generated'
import { formatCost } from '@/shared/format'
import {
  adaptAdminLibrariesPage,
  adaptLibrarySettings,
  buildWebIngestFilter,
  parsePatternLine,
} from './librariesAdapter'

const library: CatalogLibraryResponse = {
  id: 'lib-1',
  workspaceId: 'ws-1',
  slug: 'manuals',
  displayName: 'Manuals',
  description: null,
  extractionPrompt: null,
  includeDocumentHintInMcpAnswers: true,
  lifecycleState: 'active',
  ingestionReadiness: { ready: true, missingBindingPurposes: [] },
  runtimeReadiness: { missingBindingPurposes: [] },
  recognitionPolicy: { rasterImageEngine: 'vision' },
  retrievalConfig: { lexical: { textSearchConfig: 'english' } },
  webIngestPolicy: {
    crawlFilter: {
      allowPatterns: [{ kind: 'url_prefix', value: 'https://docs.example.test', source: 'import' }],
      blockPatterns: [],
    },
    materializationFilter: { allowPatterns: [], blockPatterns: [] },
  },
}

describe('libraries adapter', () => {
  it('maps list rows without inventing absent costs', () => {
    const response: AdminLibraryPageResponse = {
      items: [
        {
          id: 'lib-1',
          slug: 'manuals',
          displayName: 'Manuals',
          lifecycleState: 'active',
          includeDocumentHintInMcpAnswers: true,
          queuedJobCount: 9,
          ingestionReadiness: { ready: true, missingBindingPurposes: [] },
          workspace: { id: 'ws-1', slug: 'main', displayName: 'Main' },
        },
      ],
      totalCount: 1,
      facets: { activeCount: 1, readyCount: 1, workspaceCount: 1 },
    }

    expect(adaptAdminLibrariesPage(response).items[0]?.costSummary).toBeNull()
    expect(adaptAdminLibrariesPage(response).items[0]?.queuedJobCount).toBe(9)
  })

  it('formats partial and unknown coverage explicitly', () => {
    expect(
      formatCost(
        {
          costRollupFreshness: 'current',
          costCoverage: 'partial',
          currencyCode: 'USD',
          providerCallCount: 4,
          totalCost: '1.25',
          unpricedProviderCallCount: 3,
        },
        'Cost unknown',
        'unpriced calls',
        'en',
      ),
    ).toContain('3 unpriced calls')
    expect(formatCost(null, 'Cost unknown', 'unpriced calls', 'en')).toBe('Cost unknown')
  })

  it('adapts settings and preserves pattern provenance on an unchanged rule', () => {
    const draft = adaptLibrarySettings(library, library.retrievalConfig)
    const filter = buildWebIngestFilter(
      draft.crawlAllowPatterns,
      draft.crawlBlockPatterns,
      library.webIngestPolicy.crawlFilter,
    )

    expect(draft.recognitionEngine).toBe('vision')
    expect(draft.chunkingTemplate).toBe('naive')
    expect(filter.allowPatterns[0]?.source).toBe('import')
  })

  it('rejects unsupported pattern kinds', () => {
    expect(() =>
      buildWebIngestFilter('regex:^/private', '', {
        allowPatterns: [],
        blockPatterns: [],
      }),
    ).toThrow('regex is not a supported pattern kind')
  })
})

describe('parsePatternLine', () => {
  it('infers the kind when the line does not name one', () => {
    expect(parsePatternLine('https://docs.example.test/a')).toEqual({
      kind: 'url_prefix',
      value: 'https://docs.example.test/a',
    })
    expect(parsePatternLine('/manuals')).toEqual({ kind: 'path_prefix', value: '/manuals' })
    expect(parsePatternLine('**/*.pdf')).toEqual({ kind: 'glob', value: '**/*.pdf' })
  })

  it('reads an explicit kind but never mistakes a URL scheme for one', () => {
    expect(parsePatternLine('glob: **/*.md')).toEqual({ kind: 'glob', value: '**/*.md' })
    expect(parsePatternLine('http://a.test/x')).toEqual({
      kind: 'url_prefix',
      value: 'http://a.test/x',
    })
  })

  it('rejects an unsupported kind', () => {
    expect(() => parsePatternLine('regex:^/private')).toThrow(/not a supported pattern kind/)
  })

  it('rejects an empty value', () => {
    expect(() => parsePatternLine('glob:   ')).toThrow(/value is empty/)
  })

  it('rejects a value beyond the length ceiling', () => {
    expect(() => parsePatternLine(`glob:${'a'.repeat(2049)}`)).toThrow(/longer than 2048/)
  })

  it('demands a leading slash from a path prefix', () => {
    expect(() => parsePatternLine('path_prefix:manuals')).toThrow(/must start with \//)
  })

  it('demands an absolute URL from a url prefix', () => {
    expect(() => parsePatternLine('url_prefix:not-a-url')).toThrow(/must be an absolute URL/)
  })
})
