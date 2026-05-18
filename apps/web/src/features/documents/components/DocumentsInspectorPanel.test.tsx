import { act, type ReactElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '@/shared/i18n';
import type { DocumentLifecycleDetail } from '@/shared/api';
import { TooltipProvider } from '@/shared/components/ui/tooltip';
import type { DocumentItem } from '@/shared/types';

import { DocumentsInspectorPanel } from './DocumentsInspectorPanel';

const { toastErrorMock, toastSuccessMock, updateDocumentHintMock } = vi.hoisted(() => ({
  toastErrorMock: vi.fn(),
  toastSuccessMock: vi.fn(),
  updateDocumentHintMock: vi.fn(),
}));

vi.mock('@/shared/api', () => ({
  documentsApi: {
    updateDocumentHint: updateDocumentHintMock,
  },
}));

vi.mock('sonner', () => ({
  toast: {
    error: toastErrorMock,
    success: toastSuccessMock,
  },
}));

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
    sourceAccess: { kind: 'stored_document', href: '/v1/content/documents/doc-1/source' },
    ...overrides,
  };
}

function setInputValue(input: HTMLInputElement, value: string) {
  const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
  valueSetter?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

function withTooltipProvider(element: ReactElement) {
  return <TooltipProvider>{element}</TooltipProvider>;
}

describe('DocumentsInspectorPanel', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    updateDocumentHintMock.mockImplementation(
      async (_documentId: string, documentHint: string | null) => documentHint,
    );
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
    editorActionDisabledReason?: string | null;
    documentHintEditable?: boolean;
    editorActionEnabled?: boolean;
    editorActionReadOnly?: boolean;
    onDocumentHintUpdated?: (documentId: string, documentHint: string | null) => void;
    presentation?: 'sidebar' | 'drawer';
    selectedDoc?: DocumentItem;
  }) {
    await act(async () => {
      root = createRoot(container);
      root.render(withTooltipProvider(
        <DocumentsInspectorPanel
          documentHintEditable={overrides?.documentHintEditable ?? true}
          editorActionDisabledReason={overrides?.editorActionDisabledReason ?? null}
          editorActionEnabled={overrides?.editorActionEnabled ?? true}
          editorActionReadOnly={overrides?.editorActionReadOnly ?? false}
          formatErrorMessage={(error, fallback) =>
            error instanceof Error ? `${fallback}: ${error.message}` : fallback
          }
          lifecycle={null}
          locale="en"
          onDocumentHintUpdated={overrides?.onDocumentHintUpdated}
          onOpenEditor={noop}
          onRetry={noop}
          presentation={overrides?.presentation}
          selectedDoc={overrides?.selectedDoc ?? buildSelectedDoc()}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      ));
    });
  }

  it('renders the edit action as the first inspector action', async () => {
    await renderPanel();

    const buttons = Array.from(container.querySelectorAll('button'));
    const editButton = buttons.find(button => button.getAttribute('aria-label') === 'Edit');
    const downloadButton = buttons.find(
      button => button.getAttribute('aria-label') === 'Download',
    );

    expect(editButton).toBeTruthy();
    expect(editButton?.hasAttribute('disabled')).toBe(false);
    expect(editButton?.getAttribute('title')).toBeNull();
    expect(downloadButton).toBeTruthy();
    expect(downloadButton?.getAttribute('title')).toBeNull();
    expect(container.querySelector('[role="tooltip"]')?.textContent).toBe('Edit');
    expect(container.textContent).not.toContain('Append Text');
    expect(container.textContent).not.toContain('Download Text');
  });

  it('disables the edit action with a reason when the document is not editable', async () => {
    await renderPanel({
      editorActionDisabledReason: 'Finish processing before editing.',
      editorActionEnabled: false,
      selectedDoc: buildSelectedDoc({ readiness: 'processing', status: 'processing' }),
    });

    const buttons = Array.from(container.querySelectorAll('button'));
    const editButton = buttons.find(button => button.getAttribute('aria-label') === 'Edit');

    expect(editButton).toBeTruthy();
    expect(editButton?.getAttribute('disabled')).not.toBeNull();
    expect(editButton?.getAttribute('title')).toBeNull();
    expect(container.textContent).toContain('Finish processing before editing.');
  });

  it('keeps read-only document viewing active for non-editable ready formats', async () => {
    await renderPanel({
      editorActionReadOnly: true,
      selectedDoc: buildSelectedDoc({ fileName: 'guide.pdf', fileType: 'pdf' }),
    });

    const buttons = Array.from(container.querySelectorAll('button'));
    const viewButton = buttons.find(button => button.getAttribute('aria-label') === 'View Document');

    expect(viewButton).toBeTruthy();
    expect(viewButton?.hasAttribute('disabled')).toBe(false);
    expect(container.querySelector('[role="tooltip"]')?.textContent).toBe('View Document');
  });

  it('updates the document hint from the inline editor', async () => {
    const onDocumentHintUpdated = vi.fn();
    await renderPanel({
      onDocumentHintUpdated,
      selectedDoc: buildSelectedDoc({ documentHint: 'https://example.test/source' }),
    });

    const infoButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.getAttribute('aria-label') === i18n.t('documents.documentHintTooltip'),
    );
    expect(infoButton).toBeTruthy();

    const valueButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('https://example.test/source'),
    );
    expect(valueButton).toBeTruthy();

    await act(async () => {
      valueButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    const input = container.querySelector<HTMLInputElement>(
      `input[aria-label="${i18n.t('documents.documentHint')}"]`,
    );
    expect(input?.value).toBe('https://example.test/source');

    await act(async () => {
      if (input) setInputValue(input, '  Source label  ');
    });

    const saveButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Save',
    );
    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(updateDocumentHintMock).toHaveBeenCalledWith('doc-1', 'Source label');
    expect(onDocumentHintUpdated).toHaveBeenCalledWith('doc-1', 'Source label');
    expect(toastSuccessMock).toHaveBeenCalledWith('Hint updated');
  });

  it('sends an empty document hint as null', async () => {
    const onDocumentHintUpdated = vi.fn();
    await renderPanel({
      onDocumentHintUpdated,
      selectedDoc: buildSelectedDoc({ documentHint: 'Existing hint' }),
    });

    const valueButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Existing hint'),
    );
    await act(async () => {
      valueButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    const input = container.querySelector<HTMLInputElement>(
      `input[aria-label="${i18n.t('documents.documentHint')}"]`,
    );
    await act(async () => {
      if (input) setInputValue(input, '   ');
    });

    const saveButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Save',
    );
    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(updateDocumentHintMock).toHaveBeenCalledWith('doc-1', null);
    expect(onDocumentHintUpdated).toHaveBeenCalledWith('doc-1', null);
  });

  it('keeps the document hint read-only when editing is not allowed', async () => {
    await renderPanel({
      documentHintEditable: false,
      selectedDoc: buildSelectedDoc({ documentHint: 'https://example.test/source' }),
    });

    const link = container.querySelector<HTMLAnchorElement>('a[href="https://example.test/source"]');
    const editHintButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.getAttribute('aria-label') === 'Edit hint',
    );

    expect(link).toBeTruthy();
    expect(editHintButton).toBeUndefined();
  });

  it('shows zero progress for processing documents before the backend reports a stage percentage', async () => {
    await renderPanel({
      selectedDoc: buildSelectedDoc({
        readiness: 'processing',
        status: 'processing',
        progressPercent: undefined,
      }),
    });

    expect(container.textContent).toContain('0%');
  });

  it('hides the completed progress bar when the document is ready', async () => {
    await renderPanel({
      selectedDoc: buildSelectedDoc({
        progressPercent: 100,
        readiness: 'graph_ready',
        status: 'ready',
      }),
    });

    expect(container.textContent).toContain('Ready');
    expect(container.textContent).not.toContain('100%');
  });

  it('renders zero total lifecycle cost explicitly instead of a dash', async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(withTooltipProvider(
        <DocumentsInspectorPanel
          editorActionDisabledReason={null}
          editorActionEnabled
          editorActionReadOnly={false}
          lifecycle={{
            totalCost: '0',
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
                    estimatedCost: '0',
                    currencyCode: 'USD',
                  },
                ],
              },
            ],
          }}
          locale="en"
          onOpenEditor={noop}
          onRetry={noop}
          selectedDoc={buildSelectedDoc()}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      ));
    });

    expect(container.textContent).toContain('$0.0000');
  });

  it('renders pipeline billing values only where the backend reports them', async () => {
    const lifecycleWithDetails = {
      totalCost: '9.9999',
      currencyCode: 'USD',
      attempts: [
        {
          jobId: 'job-1',
          attemptNo: 1,
          attemptKind: 'content_mutation',
          status: 'processing',
          queueStartedAt: '2026-04-10T12:00:00Z',
          startedAt: '2026-04-10T12:00:01Z',
          finishedAt: null,
          totalCost: '7.4355',
          currencyCode: 'USD',
          totalElapsedMs: 193300,
          stageEvents: [
            {
              stage: 'extract_content',
              status: 'completed',
              startedAt: '2026-04-10T12:00:01Z',
              finishedAt: '2026-04-10T12:00:02Z',
              elapsedMs: 1500,
              providerKind: null,
              modelName: null,
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: null,
              currencyCode: null,
              details: {
                fileKind: 'pdf',
                recognition: { engine: 'docling' },
                pageCount: 42,
                extractUnitCount: 5,
              },
            },
            {
              stage: 'embed_chunk',
              status: 'completed',
              startedAt: '2026-04-10T12:00:02Z',
              finishedAt: '2026-04-10T12:01:02Z',
              elapsedMs: 60000,
              providerKind: 'provider-beta',
              modelName: 'text-embedding-3-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: '0.00000312',
              currencyCode: 'USD',
              providerCallCount: 3,
              details: {
                chunksEmbedded: 120,
                chunksReused: 0,
              },
            },
            {
              stage: 'extract_graph',
              status: 'started',
              startedAt: '2026-04-10T12:01:02Z',
              finishedAt: null,
              elapsedMs: 131800,
              providerKind: 'provider-alpha',
              modelName: 'alpha-chat-large',
              promptTokens: 1000,
              completionTokens: 100,
              totalTokens: 1100,
              estimatedCost: '7.4355',
              currencyCode: 'USD',
              providerCallCount: 7,
              details: {
                chunksProcessed: 120,
                extractedEntityCandidates: 18,
                extractedRelationCandidates: 9,
              },
            },
          ],
        },
      ],
    } as unknown as DocumentLifecycleDetail;

    await act(async () => {
      root = createRoot(container);
      root.render(withTooltipProvider(
        <DocumentsInspectorPanel
          editorActionDisabledReason={null}
          editorActionEnabled
          editorActionReadOnly={false}
          lifecycle={lifecycleWithDetails}
          locale="en"
          onOpenEditor={noop}
          onRetry={noop}
          selectedDoc={buildSelectedDoc({ readiness: 'processing', status: 'processing' })}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      ));
    });

    expect(container.querySelector('[data-testid="document-pipeline"]')).toBeTruthy();
    expect(container.querySelector('table')).toBeNull();
    expect(container.querySelectorAll('[data-testid^="pipeline-stage-tab-"]').length).toBeGreaterThanOrEqual(7);
    expect(container.textContent).toContain('$0.00000312');
    expect(container.textContent).toContain('$7.4355');
    expect(container.textContent).toContain('$9.9999');

    const extractTab = container.querySelector('[data-testid="pipeline-stage-tab-extract_content"]');
    const embedTab = container.querySelector('[data-testid="pipeline-stage-tab-embed_chunk"]');
    const graphTab = container.querySelector('[data-testid="pipeline-stage-tab-extract_graph"]');

    expect(graphTab?.getAttribute('aria-current')).toBe('step');

    await act(async () => {
      extractTab?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    const extractStage = container.querySelector('[data-testid="pipeline-stage-extract_content"]');
    expect(extractStage?.textContent).toContain('PDF');
    expect(extractStage?.textContent).toContain('docling');
    expect(extractStage?.textContent).toContain('Pages');
    expect(extractStage?.textContent).not.toContain('$');
    expect(extractStage?.textContent).not.toContain('text-embedding');
    expect(extractStage?.textContent).not.toContain('alpha-chat');

    await act(async () => {
      embedTab?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    const embedStage = container.querySelector('[data-testid="pipeline-stage-embed_chunk"]');
    expect(embedStage?.textContent).toContain('text-embedding-3-large');
    expect(embedStage?.textContent).not.toContain('embed-3-large');
    expect(embedStage?.textContent).toContain('$0.00000312');
    expect(embedStage?.textContent).toContain('Embedded');
    expect(embedStage?.textContent).toContain('Calls');

    await act(async () => {
      graphTab?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    const graphStage = container.querySelector('[data-testid="pipeline-stage-extract_graph"]');
    expect(graphStage?.textContent).toContain('alpha-chat-large');
    expect(graphStage?.textContent).toContain('$7.4355');
    expect(graphStage?.textContent).toContain('Calls');
  });

  it('does not leave finalizing marked active after the document is ready', async () => {
    const lifecycle = {
      totalCost: '0.1000',
      currencyCode: 'USD',
      attempts: [
        {
          jobId: 'job-1',
          attemptNo: 1,
          attemptKind: 'content_mutation',
          status: 'succeeded',
          queueStartedAt: '2026-04-10T12:00:00Z',
          startedAt: '2026-04-10T12:00:01Z',
          finishedAt: '2026-04-10T12:00:05Z',
          totalCost: '0.1000',
          currencyCode: 'USD',
          totalElapsedMs: 4000,
          stageEvents: [
            {
              stage: 'extract_graph',
              status: 'completed',
              startedAt: '2026-04-10T12:00:02Z',
              finishedAt: '2026-04-10T12:00:04Z',
              elapsedMs: 2000,
              providerKind: 'provider-alpha',
              modelName: 'alpha-chat-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: '0.1000',
              currencyCode: 'USD',
              providerCallCount: 2,
            },
            {
              stage: 'finalizing',
              status: 'completed',
              startedAt: '2026-04-10T12:00:04Z',
              finishedAt: '2026-04-10T12:00:05Z',
              elapsedMs: 1000,
              providerKind: null,
              modelName: null,
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: null,
              currencyCode: null,
            },
          ],
        },
      ],
    } as unknown as DocumentLifecycleDetail;

    await act(async () => {
      root = createRoot(container);
      root.render(withTooltipProvider(
        <DocumentsInspectorPanel
          editorActionDisabledReason={null}
          editorActionEnabled
          editorActionReadOnly={false}
          lifecycle={lifecycle}
          locale="en"
          onOpenEditor={noop}
          onRetry={noop}
          selectedDoc={buildSelectedDoc({ readiness: 'graph_ready', status: 'ready' })}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      ));
    });

    const finalizingTab = container.querySelector('[data-testid="pipeline-stage-tab-finalizing"]');
    const activeTabs = container.querySelectorAll('[aria-current="step"]');

    expect(finalizingTab?.getAttribute('aria-current')).toBeNull();
    expect(activeTabs).toHaveLength(0);
  });

  it('shows document-level stage costs from older lifecycle attempts on the current pipeline', async () => {
    const lifecycle = {
      totalCost: '0.3000',
      currencyCode: 'USD',
      attempts: [
        {
          jobId: 'job-2',
          attemptNo: 2,
          attemptKind: 'content_mutation',
          status: 'succeeded',
          queueStartedAt: '2026-04-10T12:05:00Z',
          startedAt: '2026-04-10T12:05:01Z',
          finishedAt: '2026-04-10T12:05:05Z',
          totalCost: null,
          currencyCode: null,
          totalElapsedMs: 4000,
          stageEvents: [
            {
              stage: 'embed_chunk',
              status: 'completed',
              startedAt: '2026-04-10T12:05:01Z',
              finishedAt: '2026-04-10T12:05:02Z',
              elapsedMs: 1000,
              providerKind: 'provider-beta',
              modelName: 'text-embedding-3-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: null,
              currencyCode: null,
            },
            {
              stage: 'extract_graph',
              status: 'completed',
              startedAt: '2026-04-10T12:05:02Z',
              finishedAt: '2026-04-10T12:05:05Z',
              elapsedMs: 3000,
              providerKind: 'provider-alpha',
              modelName: 'alpha-chat-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: null,
              currencyCode: null,
            },
          ],
        },
        {
          jobId: 'job-1',
          attemptNo: 1,
          attemptKind: 'content_mutation',
          status: 'failed',
          queueStartedAt: '2026-04-10T12:00:00Z',
          startedAt: '2026-04-10T12:00:01Z',
          finishedAt: '2026-04-10T12:00:05Z',
          totalCost: '0.3000',
          currencyCode: 'USD',
          totalElapsedMs: 4000,
          stageEvents: [
            {
              stage: 'embed_chunk',
              status: 'completed',
              startedAt: '2026-04-10T12:00:01Z',
              finishedAt: '2026-04-10T12:00:02Z',
              elapsedMs: 1000,
              providerKind: 'provider-beta',
              modelName: 'text-embedding-3-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: '0.1000',
              currencyCode: 'USD',
            },
            {
              stage: 'extract_graph',
              status: 'failed',
              startedAt: '2026-04-10T12:00:02Z',
              finishedAt: '2026-04-10T12:00:05Z',
              elapsedMs: 3000,
              providerKind: 'provider-alpha',
              modelName: 'alpha-chat-large',
              promptTokens: null,
              completionTokens: null,
              totalTokens: null,
              estimatedCost: '0.2000',
              currencyCode: 'USD',
            },
          ],
        },
      ],
    } as unknown as DocumentLifecycleDetail;

    await act(async () => {
      root = createRoot(container);
      root.render(withTooltipProvider(
        <DocumentsInspectorPanel
          editorActionDisabledReason={null}
          editorActionEnabled
          editorActionReadOnly={false}
          lifecycle={lifecycle}
          locale="en"
          onOpenEditor={noop}
          onRetry={noop}
          selectedDoc={buildSelectedDoc({ readiness: 'graph_ready', status: 'ready' })}
          selectionMode={false}
          setDeleteDocOpen={noop}
          setReplaceFileOpen={noop}
          t={i18n.t.bind(i18n)}
          updateSearchParamState={noop}
        />,
      ));
    });

    const embedTab = container.querySelector('[data-testid="pipeline-stage-tab-embed_chunk"]');
    const graphTab = container.querySelector('[data-testid="pipeline-stage-tab-extract_graph"]');

    expect(embedTab?.textContent).toContain('$0.1000');
    expect(graphTab?.textContent).toContain('$0.2000');
    expect(container.textContent).toContain('$0.3000');
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

  it('shows one explicit failed-document error from the selected document', async () => {
    await renderPanel({
      selectedDoc: buildSelectedDoc({
        status: 'failed',
        readiness: 'failed',
        failureCode: 'parser_failed',
        failureMessage: 'Parser failed on page 2',
        statusReason: 'Parser failed on page 2',
      }),
    });

    const errorBlocks = Array.from(container.querySelectorAll('.inline-error'));
    expect(errorBlocks).toHaveLength(1);
    expect(errorBlocks[0]?.textContent).not.toContain('Error');
    expect(errorBlocks[0]?.textContent).toContain('Parser failed on page 2');
  });

  it('renders the same failed-document error in the mobile drawer presentation', async () => {
    await renderPanel({
      presentation: 'drawer',
      selectedDoc: buildSelectedDoc({
        status: 'failed',
        readiness: 'failed',
        failureCode: 'parser_failed',
        failureMessage: 'Parser failed on page 2',
        statusReason: 'Parser failed on page 2',
      }),
    });

    const panel = container.firstElementChild;
    expect(panel?.className).toContain('h-full');
    expect(panel?.className).not.toContain('hidden md:block');
    expect(container.textContent).toContain('Parser failed on page 2');
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
