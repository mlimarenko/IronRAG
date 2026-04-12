import { describe, expect, it } from 'vitest';

import {
  createEditorBaseline,
  isEditorContentDirty,
  normalizeEditorMarkdown,
} from './editorBaseline';

describe('editorBaseline', () => {
  it('normalizes line endings and outer whitespace', () => {
    expect(normalizeEditorMarkdown('\r\nhello\r\nworld\r\n')).toBe('hello\nworld');
  });

  it('treats equivalent loaded and current content as clean', () => {
    const baseline = createEditorBaseline('## Title\n\nParagraph');
    expect(isEditorContentDirty(baseline, '## Title\n\nParagraph\n')).toBe(false);
  });

  it('marks real content edits as dirty', () => {
    const baseline = createEditorBaseline('## Title\n\nParagraph');
    expect(isEditorContentDirty(baseline, '## Title\n\nChanged paragraph')).toBe(true);
  });
});
