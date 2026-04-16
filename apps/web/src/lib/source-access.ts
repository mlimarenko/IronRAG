import type { SourceAccess } from '@/types';

type RawSourceAccess = {
  kind?: unknown;
  href?: unknown;
};

export function mapSourceAccess(raw: unknown): SourceAccess | undefined {
  if (!raw || typeof raw !== 'object') {
    return undefined;
  }

  const access = raw as RawSourceAccess;
  const kind = access.kind;
  const href = typeof access.href === 'string' ? access.href.trim() : '';
  if (href.length === 0) {
    return undefined;
  }
  if (kind !== 'stored_document' && kind !== 'external_url') {
    return undefined;
  }

  return { kind, href };
}
