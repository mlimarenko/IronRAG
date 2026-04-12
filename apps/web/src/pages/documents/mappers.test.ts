import { describe, expect, it } from 'vitest';

import i18n from '@/i18n';

import { formatDocumentTypeLabel } from './mappers';

describe('formatDocumentTypeLabel', () => {
  it('renders a canonical web page label for web-ingested documents', () => {
    expect(formatDocumentTypeLabel('php', 'web_page', i18n.t.bind(i18n))).toBe('Web page');
  });

  it('keeps extension-driven labels for uploaded documents', () => {
    expect(formatDocumentTypeLabel('xlsx', 'upload', i18n.t.bind(i18n))).toBe('XLSX');
  });
});
