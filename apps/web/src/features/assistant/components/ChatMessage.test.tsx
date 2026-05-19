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
});
