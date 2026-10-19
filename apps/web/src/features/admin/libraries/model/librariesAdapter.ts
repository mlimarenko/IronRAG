import type {
  AdminLibraryCostSummary,
  AdminLibraryCostValue,
  AdminLibraryPageResponse,
  AdminLibraryWorkspaceSummary,
  CatalogLibraryResponse,
  RecognitionEngine,
  RetrievalConfig,
  WebIngestPattern,
  WebIngestUrlFilter,
} from '@/shared/api/generated'

export const CHUNKING_TEMPLATES = ['naive', 'structured', 'tabular', 'qa', 'code', 'paper'] as const

type ChunkingTemplate = (typeof CHUNKING_TEMPLATES)[number]
type EditableRecognitionEngine = Extract<RecognitionEngine, 'docling' | 'vision'>

export type AdminLibraryRow = {
  id: string
  slug: string
  displayName: string
  lifecycleState: string
  queuedJobCount: number
  includeDocumentHintInMcpAnswers: boolean
  workspace: AdminLibraryWorkspaceSummary
  readiness: {
    ready: boolean
    missingBindingPurposes: string[]
  }
  costSummary: AdminLibraryCostSummary | null
  /**
   * The canonical cost projection the grid reads; null is the real NULL bucket
   * for a blocked/mixed-currency or completely unpriced rollup.
   */
  costValue?: AdminLibraryCostValue | null
}

export type AdminLibrariesPage = {
  items: AdminLibraryRow[]
  totalCount: number
  facets: AdminLibraryPageResponse['facets']
  aggregateCostSummary: AdminLibraryCostSummary | null
}

export type LibrarySettingsDraft = {
  displayName: string
  slug: string
  description: string
  lifecycleState: 'active' | 'archived'
  extractionPrompt: string
  includeDocumentHintInMcpAnswers: boolean
  recognitionEngine: EditableRecognitionEngine
  textSearchConfig: string
  chunkingTemplate: ChunkingTemplate
  crawlAllowPatterns: string
  crawlBlockPatterns: string
  materializationAllowPatterns: string
  materializationBlockPatterns: string
}

export function adaptAdminLibrariesPage(response: AdminLibraryPageResponse): AdminLibrariesPage {
  return {
    items: response.items.map((item) => ({
      id: item.id,
      slug: item.slug,
      displayName: item.displayName,
      lifecycleState: item.lifecycleState,
      queuedJobCount: item.queuedJobCount,
      includeDocumentHintInMcpAnswers: item.includeDocumentHintInMcpAnswers,
      workspace: item.workspace,
      readiness: {
        ready: item.ingestionReadiness.ready,
        missingBindingPurposes: item.ingestionReadiness.missingBindingPurposes,
      },
      costSummary: item.costSummary ?? null,
      costValue: item.costValue ?? null,
    })),
    totalCount: response.totalCount,
    facets: response.facets,
    aggregateCostSummary: response.aggregateCostSummary ?? null,
  }
}

function formatPatterns(patterns: WebIngestPattern[]): string {
  return patterns.map((pattern) => `${pattern.kind}:${pattern.value}`).join('\n')
}

export function adaptLibrarySettings(
  library: CatalogLibraryResponse,
  retrievalConfig: RetrievalConfig,
): LibrarySettingsDraft {
  return {
    displayName: library.displayName,
    slug: library.slug,
    description: library.description ?? '',
    lifecycleState: library.lifecycleState === 'active' ? 'active' : 'archived',
    extractionPrompt: library.extractionPrompt ?? '',
    includeDocumentHintInMcpAnswers: library.includeDocumentHintInMcpAnswers,
    recognitionEngine:
      library.recognitionPolicy.rasterImageEngine === 'vision' ? 'vision' : 'docling',
    textSearchConfig: retrievalConfig.lexical.textSearchConfig,
    // The public catalog DTO currently omits this persisted field. Keep the
    // product-approved enum visible without pretending the value can be saved.
    chunkingTemplate: 'naive',
    crawlAllowPatterns: formatPatterns(library.webIngestPolicy.crawlFilter.allowPatterns),
    crawlBlockPatterns: formatPatterns(library.webIngestPolicy.crawlFilter.blockPatterns),
    materializationAllowPatterns: formatPatterns(
      library.webIngestPolicy.materializationFilter.allowPatterns,
    ),
    materializationBlockPatterns: formatPatterns(
      library.webIngestPolicy.materializationFilter.blockPatterns,
    ),
  }
}

const PATTERN_KINDS = new Set(['url_prefix', 'path_prefix', 'glob'])

function inferPatternKind(value: string): string {
  if (/^https?:\/\//i.test(value)) return 'url_prefix'
  if (value.startsWith('/')) return 'path_prefix'
  return 'glob'
}

const MAX_PATTERN_VALUE_LENGTH = 2048

/**
 * Parse and validate one written filter line into a pattern.
 *
 * The whole of the "is this line acceptable" decision lives here: which kinds
 * exist, whether the kind was written explicitly or has to be inferred, and
 * what each kind demands of its value. It used to sit inside the loop that
 * also deduplicates and re-uses previous pattern objects, so none of these
 * rules could be exercised without building an array of prior patterns first.
 *
 * Throws with an operator-readable message when the line is not acceptable.
 */
export function parsePatternLine(line: string): WebIngestPattern {
  const separator = line.indexOf(':')
  const hasExplicitKind = separator > 0 && !/^https?:\/\//i.test(line)
  const kind = hasExplicitKind ? line.slice(0, separator).trim() : inferPatternKind(line)
  const value = (hasExplicitKind ? line.slice(separator + 1) : line).trim()
  if (!PATTERN_KINDS.has(kind)) throw new Error(`${kind} is not a supported pattern kind`)
  if (!value) throw new Error(`${kind} value is empty`)
  if (value.length > MAX_PATTERN_VALUE_LENGTH) {
    throw new Error(`${kind} value is longer than ${MAX_PATTERN_VALUE_LENGTH} characters`)
  }
  if (kind === 'path_prefix' && !value.startsWith('/')) {
    throw new Error('path_prefix value must start with /')
  }
  if (kind === 'url_prefix') {
    try {
      new URL(value)
    } catch {
      throw new Error('url_prefix value must be an absolute URL')
    }
  }
  return { kind, value }
}

const patternIdentity = (pattern: WebIngestPattern): string =>
  `${pattern.kind}\u0000${pattern.value}`

function parsePatternLines(text: string, previous: WebIngestPattern[]): WebIngestPattern[] {
  const previousByIdentity = new Map(previous.map((pattern) => [patternIdentity(pattern), pattern]))
  const seen = new Set<string>()
  const parsed: WebIngestPattern[] = []

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue
    const pattern = parsePatternLine(line)
    const identity = patternIdentity(pattern)
    if (seen.has(identity)) continue
    seen.add(identity)
    // Re-use the previous object so an unchanged pattern keeps its identity
    // and the form does not read as dirty.
    parsed.push(previousByIdentity.get(identity) ?? pattern)
  }
  return parsed
}

export function buildWebIngestFilter(
  allowText: string,
  blockText: string,
  previous: WebIngestUrlFilter,
): WebIngestUrlFilter {
  return {
    allowPatterns: parsePatternLines(allowText, previous.allowPatterns),
    blockPatterns: parsePatternLines(blockText, previous.blockPatterns),
  }
}
