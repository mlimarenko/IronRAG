import { act } from 'react';
import type { TFunction } from 'i18next';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { ChatMessage } from './ChatMessage';

const t = ((key: string) => key) as TFunction;

describe('ChatMessage', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  it('renders markdown source links as visible clickable links', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-1',
            role: 'assistant',
            content: 'Sources\n- [Alpha Guide](https://example.test/source)',
            timestamp: '2026-04-10T10:00:00Z',
          }}
        />,
      );
    });

    const link = container.querySelector<HTMLAnchorElement>('a[href="https://example.test/source"]');
    expect(link).toBeTruthy();
    expect(link?.textContent).toBe('Alpha Guide');
    expect(link?.target).toBe('_blank');
    expect(link?.rel).toContain('noopener');
    expect(link?.className).toContain('text-primary');
    expect(link?.className).toContain('underline');
  });

  it('keeps structured evidence sources out of the answer bubble', async () => {
    let openedEvidence = false;
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          onOpenEvidence={() => {
            openedEvidence = true;
          }}
          totalSourceCount={1}
          message={{
            id: 'assistant-2',
            role: 'assistant',
            content: 'The answer cites a source title in plain text.',
            timestamp: '2026-04-10T10:00:00Z',
            evidence: {
              segmentRefs: [
                {
                  documentId: 'doc-1',
                  documentName: 'Alpha Guide.md',
                  documentTitle: 'Alpha Guide',
                  sourceUri: 'upload://doc-1',
                  sourceAccess: {
                    kind: 'stored_document',
                    href: '/v1/content/documents/doc-1/source',
                  },
                  segmentOrdinal: 1,
                  excerpt: 'Installation',
                  relevance: 0.91,
                },
              ],
              factRefs: [],
              entityRefs: [],
              relationRefs: [],
              verificationState: 'passed',
              verificationWarnings: [],
            },
          }}
        />,
      );
    });

    expect(container.querySelector('a[href="/v1/content/documents/doc-1/source"]')).toBeNull();
    expect(container.textContent).not.toContain('Alpha Guide');
    expect(container.textContent).toContain('assistant.seeAllSources');
    expect(container.textContent).not.toContain('assistant.attachedSources');
    expect(container.textContent).not.toContain('assistant.attachedSourcesNote');
    expect(container.querySelector('hr')).toBeNull();
    const evidenceButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('assistant.seeAllSources'),
    );
    expect(evidenceButton).toBeTruthy();
    await act(async () => {
      evidenceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(openedEvidence).toBe(true);
  });

  it('keeps model-authored source links without adding structured evidence links inline', async () => {
    let openedEvidence = false;
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          onOpenEvidence={() => {
            openedEvidence = true;
          }}
          totalSourceCount={1}
          message={{
            id: 'assistant-3',
            role: 'assistant',
            content:
              'The answer body stays visible.\n\nSources:\n- [Generated Source](https://generated.example/source)',
            timestamp: '2026-04-10T10:00:00Z',
            evidence: {
              segmentRefs: [
                {
                  documentId: 'doc-1',
                  documentName: 'Alpha Guide.md',
                  documentTitle: 'Alpha Guide',
                  sourceUri: 'upload://doc-1',
                  sourceAccess: {
                    kind: 'stored_document',
                    href: '/v1/content/documents/doc-1/source',
                  },
                  segmentOrdinal: 1,
                  excerpt: 'Installation',
                  relevance: 0.91,
                },
              ],
              factRefs: [],
              entityRefs: [],
              relationRefs: [],
              verificationState: 'passed',
              verificationWarnings: [],
            },
          }}
        />,
      );
    });

    expect(container.textContent).toContain('The answer body stays visible.');
    expect(container.textContent).toContain('Generated Source');
    expect(container.querySelector('a[href="https://generated.example/source"]')).toBeTruthy();
    expect(container.textContent).toContain('assistant.seeAllSources');
    expect(container.textContent).not.toContain('assistant.attachedSources');
    expect(container.textContent).not.toContain('assistant.attachedSourcesNote');

    expect(container.querySelector('a[href="/v1/content/documents/doc-1/source"]')).toBeNull();
    const evidenceButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('assistant.seeAllSources'),
    );
    expect(evidenceButton).toBeTruthy();
    await act(async () => {
      evidenceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(openedEvidence).toBe(true);
  });

  it('does not duplicate inline model links with attached structured evidence links', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-4',
            role: 'assistant',
            content: 'See [Alpha Guide](/v1/content/documents/doc-1/source) for setup details.',
            timestamp: '2026-04-10T10:00:00Z',
            evidence: {
              segmentRefs: [
                {
                  documentId: 'doc-1',
                  documentName: 'Alpha Guide.md',
                  documentTitle: 'Alpha Guide',
                  sourceUri: 'upload://doc-1',
                  sourceAccess: {
                    kind: 'stored_document',
                    href: '/v1/content/documents/doc-1/source',
                  },
                  segmentOrdinal: 1,
                  excerpt: 'Setup details',
                  relevance: 0.91,
                },
              ],
              factRefs: [],
              entityRefs: [],
              relationRefs: [],
              verificationState: 'passed',
              verificationWarnings: [],
            },
          }}
        />,
      );
    });

    const matchingLinks = container.querySelectorAll<HTMLAnchorElement>(
      'a[href="/v1/content/documents/doc-1/source"]',
    );
    expect(matchingLinks).toHaveLength(1);
    expect(matchingLinks[0]?.textContent).toBe('Alpha Guide');
    expect(container.textContent).not.toContain('assistant.sources');
    expect(container.textContent).not.toContain('assistant.attachedSources');
    expect(container.textContent).not.toContain('assistant.attachedSourcesNote');
  });

  it('renders markdown images as safe links instead of embedded media', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-image',
            role: 'assistant',
            content: 'Open ![diagram](https://example.test/diagram.png) for details.',
            timestamp: '2026-04-10T10:00:00Z',
          }}
        />,
      );
    });

    expect(container.querySelector('img')).toBeNull();
    const link = container.querySelector<HTMLAnchorElement>(
      'a[href="https://example.test/diagram.png"]',
    );
    expect(link).toBeTruthy();
    expect(link?.textContent).toBe('diagram');
    expect(link?.target).toBe('_blank');
  });

  it('keeps prose after a separator when it is not a bare source list', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-5',
            role: 'assistant',
            content:
              'The answer has a separate note.\n\n---\nConclusion\nAlpha Guide covers setup details.',
            timestamp: '2026-04-10T10:00:00Z',
            evidence: {
              segmentRefs: [
                {
                  documentId: 'doc-1',
                  documentName: 'Alpha Guide.md',
                  documentTitle: 'Alpha Guide',
                  sourceUri: 'upload://doc-1',
                  sourceAccess: {
                    kind: 'stored_document',
                    href: '/v1/content/documents/doc-1/source',
                  },
                  segmentOrdinal: 1,
                  excerpt: 'Setup details',
                  relevance: 0.91,
                },
              ],
              factRefs: [],
              entityRefs: [],
              relationRefs: [],
              verificationState: 'passed',
              verificationWarnings: [],
            },
          }}
        />,
      );
    });

    expect(container.textContent).toContain('Conclusion');
    expect(container.textContent).toContain('Alpha Guide covers setup details.');
    expect(container.querySelector('hr')).toBeTruthy();
  });
});
