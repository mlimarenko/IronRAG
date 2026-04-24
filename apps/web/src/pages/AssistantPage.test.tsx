import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import AssistantPage from '@/pages/AssistantPage';

const { useAppMock, queryApiMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  queryApiMock: {
    listSessions: vi.fn(),
    getSession: vi.fn(),
    createSession: vi.fn(),
    createTurnWithFallback: vi.fn(),
    getExecution: vi.fn(),
    getRuntimeExecution: vi.fn(),
    getExecutionLlmContext: vi.fn(),
    recoverTurnAfterStreamFailure: vi.fn(),
  },
}));

vi.mock('@/contexts/AppContext', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/api', () => ({
  queryApi: queryApiMock,
}));

// ReactMarkdown is heavy to import in a jsdom environment and its output is
// not what these integration tests are validating — they check message plumbing,
// streaming state, and evidence panel wiring. Replace it with a plain `<div>`.
vi.mock('react-markdown', () => ({
  default: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="md">{children}</div>
  ),
}));

describe('AssistantPage integration', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeLibrary: {
        id: 'library-1',
        workspaceId: 'ws-1',
        missingBindingPurposes: [],
      },
      activeWorkspace: { id: 'ws-1' },
      locale: 'en',
    });

    queryApiMock.listSessions.mockResolvedValue([
      { id: 'session-1', libraryId: 'library-1', title: 'Deployment notes', updatedAt: '2026-04-10T10:00:00Z', turnCount: 2 },
    ]);
    queryApiMock.getSession.mockResolvedValue({
      id: 'session-1',
      libraryId: 'library-1',
      title: 'Deployment notes',
      updatedAt: '2026-04-10T10:00:00Z',
      turnCount: 2,
      messages: [],
    });
    queryApiMock.createSession.mockResolvedValue({
      id: 'session-new',
      libraryId: 'library-1',
      title: '',
      updatedAt: '2026-04-10T11:00:00Z',
      turnCount: 0,
    });
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  async function renderPage() {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={['/assistant']}>
          <AssistantPage />
        </MemoryRouter>,
      );
    });
    await flushUi();
    await flushUi();
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll('button')).find((b) =>
      b.textContent?.includes(text),
    );
  }

  function setTextareaValue(value: string) {
    const textarea = container.querySelector('textarea') as HTMLTextAreaElement | null;
    expect(textarea).toBeTruthy();
    const descriptor = Object.getOwnPropertyDescriptor(
      window.HTMLTextAreaElement.prototype,
      'value',
    );
    descriptor?.set?.call(textarea, value);
    textarea?.dispatchEvent(new Event('input', { bubbles: true }));
  }

  it('loads the session rail on mount and renders session titles', async () => {
    await renderPage();

    expect(queryApiMock.listSessions).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      libraryId: 'library-1',
    });
    expect(container.textContent).toContain('Deployment notes');
  });

  it('streams a turn through onDelta and replaces the placeholder with the final answer + evidence', async () => {
    queryApiMock.createTurnWithFallback.mockImplementation(async (_sessionId, _q, handlers) => {
      handlers.onRuntime?.({ runtimeExecutionId: 'runtime-1' });
      handlers.onDelta?.('Hello');
      handlers.onDelta?.(' world');
      return {
        responseTurn: {
          id: 'turn-1',
          contentText: 'Hello world',
          createdAt: '2026-04-10T11:00:05Z',
          executionId: 'exec-1',
        },
        preparedSegmentReferences: [
          {
            documentId: 'doc-1',
            segmentId: 'seg-1',
            documentTitle: 'Deployment Guide',
            sourceUri: null,
            sourceAccess: null,
            headingTrail: ['Deployment', 'Production'],
            sectionPath: [],
            blockKind: 'heading',
            rank: 1,
            score: 0.91,
          },
        ],
        technicalFactReferences: [],
        entityReferences: [],
        relationReferences: [],
        verificationState: 'passed',
        verificationWarnings: [],
        runtimeStageSummaries: [],
      };
    });

    await renderPage();

    setTextareaValue('Where is the docs page?');
    await flushUi();

    const sendButton = Array.from(container.querySelectorAll('button')).find(
      (b) => b.getAttribute('disabled') === null && b.querySelector('svg'),
    ) as HTMLButtonElement | undefined;
    // The send button is the icon button at the end of the composer — fall
    // back to pressing Enter if we cannot uniquely identify it.
    if (sendButton && !sendButton.textContent?.trim()) {
      await act(async () => {
        sendButton.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
    } else {
      const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
      await act(async () => {
        textarea.dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
        );
      });
    }

    await flushUi();
    await flushUi();
    await flushUi();

    expect(queryApiMock.createSession).toHaveBeenCalledWith('ws-1', 'library-1');
    expect(queryApiMock.createTurnWithFallback).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain('Hello world');
    // Evidence panel renders the verification badge + segment ref title.
    expect(container.textContent).toContain('Deployment Guide');
  });

  it('recovers the persisted answer after a late stream interruption', async () => {
    queryApiMock.createTurnWithFallback.mockImplementation(async (_sessionId, _q, handlers) => {
      handlers.onRuntime?.({ runtimeExecutionId: 'runtime-1' });
      throw new Error('Error in input stream');
    });
    queryApiMock.recoverTurnAfterStreamFailure.mockResolvedValue({
      execution: {
        id: 'exec-1',
        runtimeExecutionId: 'runtime-1',
        lifecycleState: 'completed',
        completedAt: '2026-04-10T11:00:05Z',
      },
      responseTurn: {
        id: 'turn-1',
        contentText: 'Recovered answer',
        createdAt: '2026-04-10T11:00:05Z',
        executionId: 'exec-1',
      },
      preparedSegmentReferences: [
        {
          documentId: 'doc-1',
          segmentId: 'seg-1',
          documentTitle: 'Recovered Guide',
          sourceUri: null,
          sourceAccess: null,
          headingTrail: ['Recovery', 'Guide'],
          sectionPath: [],
          blockKind: 'heading',
          rank: 1,
          score: 0.91,
        },
      ],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'passed',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await renderPage();

    setTextareaValue('Where is the docs page?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
    });

    await flushUi();
    await flushUi();
    await flushUi();

    expect(queryApiMock.recoverTurnAfterStreamFailure).toHaveBeenCalledWith(
      'runtime-1',
    );
    expect(container.textContent).toContain('Recovered answer');
    expect(container.textContent).toContain('Recovered Guide');
    expect(container.textContent).not.toContain('Error in input stream');
  });

  it('shows the query-not-configured empty state when the active library lacks the binding', async () => {
    useAppMock.mockReturnValue({
      activeLibrary: {
        id: 'library-1',
        workspaceId: 'ws-1',
        missingBindingPurposes: ['query_answer'],
      },
      activeWorkspace: { id: 'ws-1' },
      locale: 'en',
    });

    await renderPage();

    // The page shows the "query not configured" empty state; the composer
    // textarea is absent because the main thread never mounts.
    expect(container.querySelector('textarea')).toBeNull();
    expect(container.textContent?.toLowerCase()).toContain('query');
  });

  it('opens a selected session and hydrates its messages into the thread', async () => {
    queryApiMock.getSession.mockResolvedValue({
      id: 'session-1',
      libraryId: 'library-1',
      title: 'Deployment notes',
      updatedAt: '2026-04-10T10:00:00Z',
      turnCount: 2,
      messages: [
        {
          id: 'msg-user',
          role: 'user',
          content: 'What changed in deploy?',
          timestamp: '2026-04-10T10:00:01Z',
        },
        {
          id: 'msg-assistant',
          role: 'assistant',
          content: 'We moved to keyset pagination.',
          timestamp: '2026-04-10T10:00:02Z',
          executionId: 'exec-prev',
          evidence: {
            preparedSegmentReferences: [
              {
                documentId: 'doc-1',
                segmentId: 'seg-1',
                documentTitle: 'Pagination Design',
                sourceUri: null,
                sourceAccess: null,
                headingTrail: ['Pagination', 'Design'],
                sectionPath: [],
                blockKind: 'heading',
                rank: 1,
                score: 0.91,
              },
            ],
            technicalFactReferences: [],
            entityReferences: [],
            relationReferences: [],
            verificationState: 'passed',
            verificationWarnings: [],
            runtimeStageSummaries: [],
          },
        },
      ],
    });

    await renderPage();

    const sessionButton = findButton('Deployment notes');
    expect(sessionButton).toBeTruthy();

    await act(async () => {
      sessionButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    expect(queryApiMock.getSession).toHaveBeenCalledWith('session-1');
    expect(container.textContent).toContain('We moved to keyset pagination');
    expect(container.textContent).toContain('Pagination Design');
  });
});
