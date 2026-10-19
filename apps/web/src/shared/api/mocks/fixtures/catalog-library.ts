import type { CatalogLibraryResponse } from '../../generated'

export function catalogLibrary(
  overrides: Partial<CatalogLibraryResponse> = {},
): CatalogLibraryResponse {
  return {
    id: 'library-demo-1',
    workspaceId: 'workspace-alpha',
    displayName: 'Default Library',
    slug: 'default-library',
    description: null,
    extractionPrompt: null,
    lifecycleState: 'active',
    includeDocumentHintInMcpAnswers: false,
    ingestionReadiness: { ready: true, missingBindingPurposes: [] },
    runtimeReadiness: { missingBindingPurposes: [] },
    recognitionPolicy: { rasterImageEngine: 'vision' },
    retrievalConfig: { lexical: { textSearchConfig: 'english' } },
    webIngestPolicy: {
      crawlFilter: { allowPatterns: [], blockPatterns: [] },
      materializationFilter: { allowPatterns: [], blockPatterns: [] },
    },
    ...overrides,
  }
}
