import type { SourceAccess } from '@/types';

export function mapSourceAccess(raw: any): SourceAccess | undefined {
  if (!raw || typeof raw !== 'object') {
    return undefined;
  }

  const kind = raw.kind;
  const href = typeof raw.href === 'string' ? raw.href.trim() : '';
  if (href.length === 0) {
    return undefined;
  }
  if (kind !== 'stored_document' && kind !== 'external_url') {
    return undefined;
  }

  return { kind, href };
}
