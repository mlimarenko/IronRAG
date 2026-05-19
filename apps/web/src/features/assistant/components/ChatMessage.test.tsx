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

  it('renders structured evidence sources as visible clickable links', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-2',
            role: 'assistant',
            content: 'The answer cites a source title in plain text.\n\n---\nSources\nAlpha Guide',
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

    const sourceLink = container.querySelector<HTMLAnchorElement>(
      'a[href="/v1/content/documents/doc-1/source"]',
    );
    expect(sourceLink).toBeTruthy();
    expect(sourceLink?.textContent).toContain('Alpha Guide');
    expect(sourceLink?.target).toBe('_blank');
    expect(sourceLink?.rel).toContain('noopener');
    expect(sourceLink?.className).toContain('text-primary');
    expect(sourceLink?.className).toContain('underline');
    expect(container.querySelector('hr')).toBeNull();
  });

  it('keeps prose after a separator when it is not a bare source list', async () => {
    await act(async () => {
      root?.render(
        <ChatMessage
          t={t}
          message={{
            id: 'assistant-3',
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
