import type { AssistantContentSourceAccess, ContentSourceAccess } from '@/shared/api/generated'
import type { SourceAccess } from '@/shared/types'

type SourceAccessTransport =
  AssistantContentSourceAccess | ContentSourceAccess | SourceAccess | null | undefined

export function mapSourceAccess(raw: SourceAccessTransport): SourceAccess | undefined {
  if (!raw) {
    return undefined
  }

  const { kind } = raw
  if (typeof raw.href !== 'string') {
    return undefined
  }
  const href = raw.href.trim()
  if (href.length === 0) {
    return undefined
  }
  if (kind !== 'stored_document' && kind !== 'external_url') {
    return undefined
  }

  return { kind, href }
}
