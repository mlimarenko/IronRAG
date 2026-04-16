import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '@/i18n';
import type { DocumentItem } from '@/types';

import { DocumentsInspectorPanel } from './DocumentsInspectorPanel';

const noop = vi.fn();

function buildSelectedDoc(overrides: Partial<DocumentItem> = {}): DocumentItem {
  return {
    id: 'doc-1',
    fileName: 'inventory.xlsx',
    fileType: 'xlsx',
    fileSize: 2048,
    uploadedAt: '2026-04-10T12:00:00Z',
    cost: 0.42,
    status: 'ready',
    readiness: 'graph_ready',
    stage: 'Preparing structure',
    canRetry: false,
    sourceKind: 'upload',
    sourceUri: undefined,
    sourceAccess: { kind: 'stored_document', href: '/v1/content/documents/doc-1/source' },
    ...overrides,
  };
}

describe('DocumentsInspectorPanel', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  async function renderPanel(overrides?: {
    canEdit?: boolean;
    editDisabledReason?: string | null;
    selectedDoc?: DocumentItem;
  }) {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <DocumentsInspectorPanel
          canEdit={overrides?.canEdit ?? true}
          editDisabledReason={overrides?.editDisabledReason ?? null}
          inspectorFacts={12}
          inspectorSegments={24}
          lifecycle={null}
          locale="en"
          onEdit={noop}
          onRetry={noop}
          selectedDoc={overrides?.selectedDoc ?? buildSelectedDoc()}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      );
    });
  }

  it('renders the edit action as the first inspector action', async () => {
    await renderPanel();

    const buttons = Array.from(container.querySelectorAll('button'));
    const editButton = buttons.find(button => button.textContent?.includes('Edit'));
    const downloadButton = buttons.find(button => button.textContent?.includes('Download'));

    expect(editButton).toBeTruthy();
    expect(editButton?.hasAttribute('disabled')).toBe(false);
    expect(downloadButton).toBeTruthy();
    expect(container.textContent).not.toContain('Append Text');
    expect(container.textContent).not.toContain('Download Text');
  });

  it('disables the edit action with a reason when the document is not editable', async () => {
    await renderPanel({
      canEdit: false,
      editDisabledReason: 'Finish processing before editing.',
      selectedDoc: buildSelectedDoc({ readiness: 'processing', status: 'processing' }),
    });

    const buttons = Array.from(container.querySelectorAll('button'));
    const editButton = buttons.find(button => button.textContent?.includes('Edit'));

    expect(editButton).toBeTruthy();
    expect(editButton?.getAttribute('disabled')).not.toBeNull();
    expect(editButton?.getAttribute('title')).toBe('Finish processing before editing.');
  });

  it('renders zero total lifecycle cost explicitly instead of a dash', async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <DocumentsInspectorPanel
          canEdit
          editDisabledReason={null}
          inspectorFacts={12}
          inspectorSegments={24}
          lifecycle={{
            totalCost: 0,
            currencyCode: 'USD',
            attempts: [
              {
                jobId: 'job-1',
                attemptNo: 1,
                attemptKind: 'content_mutation',
                status: 'succeeded',
                queueStartedAt: '2026-04-10T12:00:00Z',
                startedAt: '2026-04-10T12:00:01Z',
                finishedAt: '2026-04-10T12:00:02Z',
                totalElapsedMs: 1000,
                stageEvents: [
                  {
                    stage: 'extract_content',
                    status: 'completed',
                    startedAt: '2026-04-10T12:00:01Z',
                    finishedAt: '2026-04-10T12:00:02Z',
                    elapsedMs: 1000,
                    providerKind: null,
                    modelName: null,
                    promptTokens: null,
                    completionTokens: null,
                    totalTokens: null,
                    estimatedCost: 0,
                    currencyCode: 'USD',
                  },
                ],
              },
            ],
          }}
          locale="en"
          onEdit={noop}
          onRetry={noop}
          selectedDoc={buildSelectedDoc()}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      );
    });

    expect(container.textContent).toContain('$0.0000');
  });

  it('renders web-ingested documents with a web page type label', async () => {
    await renderPanel({
      selectedDoc: buildSelectedDoc({
        fileName: 'index.php',
        fileType: 'php',
        sourceKind: 'web_page',
        sourceUri: 'https://ru.wikipedia.org/wiki/Test',
        sourceAccess: { kind: 'external_url', href: 'https://ru.wikipedia.org/wiki/Test' },
      }),
    });

    expect(container.textContent).toContain('Web page');
    expect(container.textContent).not.toContain('PHP');
  });

  it('collapses long inspector titles behind an explicit toggle', async () => {
    const fullUrl =
      'https://passport.yandex.ru/showcaptcha?cc=1&from=fb-hint=8.191&mt=895CC538B346D26B47C082D2499B07E478D7FB2AE8D408D1DE014386C74C5D639';

    await renderPanel({
      selectedDoc: buildSelectedDoc({
        fileName: fullUrl,
        fileType: 'php',
        sourceKind: 'web_page',
      }),
    });

    expect(container.textContent).toContain('Show full name');
    expect(container.textContent).toContain('https://passport.yandex.ru/showcaptcha?');
    expect(container.textContent).not.toContain(fullUrl);

    const toggleButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('Show full name'),
    );

    expect(toggleButton).toBeTruthy();

    await act(async () => {
      toggleButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(container.textContent).toContain('Show less');
    expect(container.textContent).toContain(fullUrl);
  });
});
