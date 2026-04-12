import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '@/i18n';
import type { DocumentItem, DocumentReadiness } from '@/types';

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

const readinessConfig: Record<DocumentReadiness, { label: string; cls: string }> = {
  processing: { label: 'Processing', cls: 'status-processing' },
  readable: { label: 'Readable', cls: 'status-warning' },
  graph_sparse: { label: 'Graph Sparse', cls: 'status-warning' },
  graph_ready: { label: 'Graph Ready', cls: 'status-ready' },
  failed: { label: 'Failed', cls: 'status-failed' },
};

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
          readinessConfig={readinessConfig}
          selectedDoc={overrides?.selectedDoc ?? buildSelectedDoc()}
          selectionMode={false}
          setAddLinkOpen={noop}
          setCrawlMode={noop}
          setDeleteDocOpen={noop}
          setMaxDepth={noop}
          setMaxPages={noop}
          setReplaceFileOpen={noop}
          setSeedUrl={noop}
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
          readinessConfig={readinessConfig}
          selectedDoc={buildSelectedDoc()}
          selectionMode={false}
          setAddLinkOpen={noop}
          setCrawlMode={noop}
          setDeleteDocOpen={noop}
          setMaxDepth={noop}
          setMaxPages={noop}
          setReplaceFileOpen={noop}
          setSeedUrl={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      );
    });

    expect(container.textContent).toContain('$0.0000');
  });
});
