import i18n from '@/shared/i18n'
import type {
  AiBindingPurpose,
  CatalogLibraryResponse,
  LibrarySummary,
} from '@/shared/api/generated'
import type { Library, Locale } from '@/shared/types'

const QUERY_READY_PURPOSES = new Set<AiBindingPurpose>([
  'embed_chunk',
  'query_compile',
  'query_answer',
  'agent',
])

function localizedLibraryName(slug: string, fallbackName: string, locale: Locale): string {
  return slug === 'default-library'
    ? i18n.t('shell.defaultLibraryLabel', { lng: locale })
    : fallbackName
}

function isLibraryQueryReady(
  ingestionReady: boolean,
  queryReady: boolean | null | undefined,
  missingBindingPurposes: AiBindingPurpose[],
): boolean {
  return (
    queryReady ??
    (ingestionReady && !missingBindingPurposes.some((purpose) => QUERY_READY_PURPOSES.has(purpose)))
  )
}

export function mapShellLibrary(library: LibrarySummary, locale: Locale): Library {
  const missingBindingPurposes = library.missingBindingPurposes
  return {
    id: library.id,
    workspaceId: library.workspaceId,
    name: localizedLibraryName(library.slug, library.name, locale),
    createdAt: '',
    includeDocumentHintInMcpAnswers: true,
    ingestionReady: library.ingestionReady,
    queryReady: isLibraryQueryReady(
      library.ingestionReady,
      library.queryReady,
      missingBindingPurposes,
    ),
    missingBindingPurposes,
  }
}

export function mapCatalogLibrary(library: CatalogLibraryResponse, locale: Locale): Library {
  const missingBindingPurposes = library.runtimeReadiness.missingBindingPurposes
  return {
    id: library.id,
    workspaceId: library.workspaceId,
    name: localizedLibraryName(library.slug, library.displayName, locale),
    createdAt: '',
    includeDocumentHintInMcpAnswers: library.includeDocumentHintInMcpAnswers,
    ingestionReady: library.ingestionReadiness.ready,
    queryReady: isLibraryQueryReady(
      library.ingestionReadiness.ready,
      undefined,
      missingBindingPurposes,
    ),
    missingBindingPurposes,
  }
}
