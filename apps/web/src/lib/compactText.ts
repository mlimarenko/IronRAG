export type CompactText = {
  text: string;
  fullText: string;
  isTruncated: boolean;
};

export function compactText(value: string | null | undefined, maxLength: number): CompactText {
  const fullText = typeof value === 'string' ? value.replace(/\s+/g, ' ').trim() : '';
  if (!fullText) {
    return { text: '', fullText: '', isTruncated: false };
  }

  if (fullText.length <= maxLength) {
    return { text: fullText, fullText, isTruncated: false };
  }

  return {
    text: `${fullText.slice(0, Math.max(1, maxLength - 1)).trimEnd()}…`,
    fullText,
    isTruncated: true,
  };
}

export function truncatedTitle(compact: CompactText): string | undefined {
  return compact.isTruncated ? compact.fullText : undefined;
}
