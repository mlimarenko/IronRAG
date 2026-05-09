export type DirtyStateBaseline = {
  loadedMarkdown: string;
  normalizedLoadedMarkdown: string;
};

export function normalizeEditorMarkdown(markdown: string): string {
  return markdown.replace(/\r\n?/g, '\n').trim();
}

export function createEditorBaseline(markdown: string): DirtyStateBaseline {
  return {
    loadedMarkdown: markdown,
    normalizedLoadedMarkdown: normalizeEditorMarkdown(markdown),
  };
}

export function isEditorContentDirty(
  baseline: DirtyStateBaseline | null,
  currentMarkdown: string,
): boolean {
  if (!baseline) {
    return false;
  }
  return normalizeEditorMarkdown(currentMarkdown) !== baseline.normalizedLoadedMarkdown;
}
