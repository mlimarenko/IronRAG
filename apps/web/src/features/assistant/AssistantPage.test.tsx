import { act } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { waitFor } from '@testing-library/react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import AssistantPage from '@/features/assistant/AssistantPage';
import { ApiError } from '@/shared/api/runtime';

const { useAppMock, queryApiMock, toastErrorMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  toastErrorMock: vi.fn(),
  queryApiMock: {
    listSessions: vi.fn(),
    getSession: vi.fn(),
    createSession: vi.fn(),
    createTurn: vi.fn(),
    createTurnStream: vi.fn(),
    getExecution: vi.fn(),
    getExecutionLlmContext: vi.fn(),
  },
}));

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('sonner', () => ({
  toast: {
    error: toastErrorMock,
    success: vi.fn(),
  },
}));

vi.mock('@/shared/api', () => ({
  queryApi: queryApiMock,
  queries: {
    listQuerySessionsOptions: (input?: { query?: { libraryId?: string } }) => ({
      queryKey: ['mockedListQuerySessions', input?.query?.libraryId ?? null],
      queryFn: async () =>
        queryApiMock.listSessions({ workspaceId: 'ws-1', libraryId: input?.query?.libraryId }),
    }),
    getQuerySessionOptions: (input: { path: { sessionId: string } }) => ({
      queryKey: ['mockedGetQuerySession', input.path.sessionId],
      queryFn: async () => queryApiMock.getSession(input.path.sessionId),
    }),
  },
}));

// ReactMarkdown is heavy to import in a jsdom environment and its output is
// not what these integration tests are validating — they check message plumbing,
// turn completion, and evidence panel wiring. Replace it with a plain `<div>`.
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
    window.localStorage.clear();
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
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [],
    });
    queryApiMock.createSession.mockResolvedValue({
      id: 'session-new',
      libraryId: 'library-1',
      title: '',
      updatedAt: '2026-04-10T11:00:00Z',
      turnCount: 0,
    });
    queryApiMock.createTurnStream.mockResolvedValue({
      responseTurn: {
        id: 'turn-default',
        contentText: 'Default answer',
        createdAt: '2026-04-10T11:00:05Z',
        executionId: 'exec-default',
      },
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    window.localStorage.clear();
    container.remove();
  });

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  function makeQueryClient() {
    return new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0, refetchOnWindowFocus: false } },
    });
  }

  async function renderPage() {
    const queryClient = makeQueryClient();
    await act(async () => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={['/assistant']}>
            <AssistantPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await flushUi();
    await flushUi();
    return queryClient;
  }

  async function rerenderPage() {
    const queryClient = makeQueryClient();
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={['/assistant']}>
            <AssistantPage />
          </MemoryRouter>
        </QueryClientProvider>,
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
    const textarea = container.querySelector('textarea');
    expect(textarea).toBeTruthy();
    const descriptor = Object.getOwnPropertyDescriptor(
      window.HTMLTextAreaElement.prototype,
      'value',
    );
    descriptor?.set?.call(textarea, value);
    textarea?.dispatchEvent(new Event('input', { bubbles: true }));
  }

  function visibleTextOccurrences(text: string) {
    const content = container.textContent ?? '';
    return content.split(text).length - 1;
  }

  it('loads the session rail on mount and renders session titles', async () => {
    await renderPage();

    expect(queryApiMock.listSessions).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      libraryId: 'library-1',
    });
    expect(container.textContent).toContain('Deployment notes');
  });

  it('does not render the composer debug button for admin users', async () => {
    useAppMock.mockReturnValue({
      activeLibrary: {
        id: 'library-1',
        workspaceId: 'ws-1',
        missingBindingPurposes: [],
      },
      activeWorkspace: { id: 'ws-1' },
      locale: 'en',
      user: { role: 'admin' },
    });

    await renderPage();

    expect(container.querySelector('button[aria-label="Debug"]')).toBeNull();
  });

  it('posts a turn and replaces the placeholder with the final answer + evidence', async () => {
    queryApiMock.createTurnStream.mockResolvedValue({
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
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await renderPage();

    setTextareaValue('Where is the docs page?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    const sendButton = textarea.parentElement?.querySelector('button');
    // The send button is the icon button at the end of the composer — fall
    // back to pressing Enter if we cannot uniquely identify it.
    if (sendButton && sendButton.getAttribute('disabled') === null) {
      await act(async () => {
        sendButton.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
    } else {
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
    expect(queryApiMock.createTurnStream).toHaveBeenCalledWith(
      'session-new',
      'Where is the docs page?',
      0,
      expect.any(Function),
    );
    expect(container.textContent).toContain('Hello world');
  });

  it('keeps a failed stream turn inline with the pending question and retry affordance', async () => {
    queryApiMock.createTurnStream.mockRejectedValue(new Error('Failed to fetch'));

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

    expect(queryApiMock.createTurnStream).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain('Where is the docs page?');
    expect(container.textContent).toContain('The request did not complete');
    expect(container.textContent).toContain("Request didn't go through");
  });

  it('keeps a first-turn session creation failure visible with an inline error', async () => {
    let rejectSession!: (reason: Error) => void;
    queryApiMock.createSession.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectSession = reject;
      }),
    );

    await renderPage();

    setTextareaValue('Will this rollback?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
    });
    await flushUi();

    expect(container.textContent).toContain('Will this rollback?');

    await act(async () => {
      rejectSession(new Error('session unavailable'));
    });
    await flushUi();
    await flushUi();

    expect(container.textContent).toContain('Will this rollback?');
    expect(container.textContent).toContain('The request did not complete');
    expect(container.textContent).toContain("Request didn't go through");
    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining('session unavailable'),
    );
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
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
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
                sourceAccess: { kind: 'stored_document', href: '/documents/doc-1' },
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
            verificationState: 'verified',
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
    expect(
      window.localStorage.getItem(
        'ironrag_assistant_active_session:ws-1:library-1',
      ),
    ).toBe(JSON.stringify('session-1'));
  });

  it('restores the active session after a page reload', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [
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
                sourceAccess: { kind: 'stored_document', href: '/documents/doc-1' },
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
            verificationState: 'verified',
            verificationWarnings: [],
            runtimeStageSummaries: [{ stageKind: 'retrieve', stageLabel: 'Retrieve' }],
          },
        },
      ],
    });

    await renderPage();

    await waitFor(() => {
      expect(queryApiMock.getSession).toHaveBeenCalledWith('session-1');
    });
    expect(container.textContent).toContain('We moved to keyset pagination');
    expect(container.textContent).toContain('See 1 source');
  });

  it('renders a server-hydrated pending assistant turn after returning mid-execution', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 1,
      },
      messages: [
        {
          id: 'msg-user',
          role: 'user',
          content: 'Question still running?',
          timestamp: '2026-04-10T10:00:01Z',
        },
        {
          id: 'exec-running',
          role: 'assistant',
          content: '',
          timestamp: '2026-04-10T10:00:02Z',
          executionId: 'exec-running',
        },
      ],
    });

    await renderPage();

    await waitFor(() => {
      expect(queryApiMock.getSession).toHaveBeenCalledWith('session-1');
    });
    expect(container.textContent).toContain('Question still running?');
    expect(container.textContent).toContain('Agent turn started');
    expect(container.textContent).toContain('Agent turn is running');
  });

  it('keeps polling a restored pending turn until the durable answer appears', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    let getSessionCalls = 0;
    queryApiMock.getSession.mockImplementation(async () => {
      getSessionCalls += 1;
      return {
        session: {
          id: 'session-1',
          libraryId: 'library-1',
          title: 'Deployment notes',
          updatedAt: '2026-04-10T10:00:00Z',
          turnCount: getSessionCalls === 1 ? 1 : 2,
        },
        messages: getSessionCalls === 1
          ? [
              {
                id: 'msg-user',
                role: 'user',
                content: 'Question still running?',
                timestamp: '2026-04-10T10:00:01Z',
              },
              {
                id: 'exec-running',
                role: 'assistant',
                content: '',
                timestamp: '2026-04-10T10:00:02Z',
                executionId: 'exec-running',
              },
            ]
          : [
              {
                id: 'msg-user',
                role: 'user',
                content: 'Question still running?',
                timestamp: '2026-04-10T10:00:01Z',
              },
              {
                id: 'msg-assistant',
                role: 'assistant',
                content: 'Recovered durable answer',
                timestamp: '2026-04-10T10:00:08Z',
                executionId: 'exec-running',
                evidence: {
                  preparedSegmentReferences: [],
                  technicalFactReferences: [],
                  entityReferences: [],
                  relationReferences: [],
                  verificationState: 'verified',
                  verificationWarnings: [],
                  runtimeStageSummaries: [],
                },
              },
            ],
      };
    });

    await renderPage();

    await waitFor(() => {
      expect(container.textContent).toContain('Agent turn is running');
    });
    await waitFor(
      () => {
        expect(queryApiMock.getSession).toHaveBeenCalledTimes(2);
      },
      { timeout: 2500 },
    );
    await waitFor(() => {
      expect(container.textContent).toContain('Recovered durable answer');
    });
    expect(container.textContent).not.toContain('Agent turn is running');
  });

  it('does not loop global notifications when the optional debug context is missing', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    window.localStorage.setItem('ironrag_assistant_debug_open', JSON.stringify(true));
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [
        {
          id: 'msg-user',
          role: 'user',
          content: 'What changed?',
          timestamp: '2026-04-10T10:00:01Z',
        },
        {
          id: 'msg-assistant',
          role: 'assistant',
          content: 'A stable answer',
          timestamp: '2026-04-10T10:00:05Z',
          executionId: 'exec-no-context',
        },
      ],
    });
    queryApiMock.getExecutionLlmContext.mockRejectedValue(
      new ApiError(404, { error: 'not found' }),
    );

    await renderPage();

    await waitFor(() => {
      expect(queryApiMock.getExecutionLlmContext).toHaveBeenCalledTimes(1);
    });
    await flushUi();
    await flushUi();

    expect(queryApiMock.getExecutionLlmContext).toHaveBeenCalledTimes(1);
    expect(toastErrorMock).not.toHaveBeenCalled();
    expect(container.textContent).toContain('LLM context was not recorded');
  });

  it('clears stale debug-context errors while a new assistant turn is pending', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    window.localStorage.setItem('ironrag_assistant_debug_open', JSON.stringify(true));
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [
        {
          id: 'msg-user',
          role: 'user',
          content: 'What changed?',
          timestamp: '2026-04-10T10:00:01Z',
        },
        {
          id: 'msg-assistant',
          role: 'assistant',
          content: 'A stable answer',
          timestamp: '2026-04-10T10:00:05Z',
          executionId: 'exec-no-context',
        },
      ],
    });
    queryApiMock.getExecutionLlmContext.mockRejectedValue(
      new ApiError(404, { error: 'not found' }),
    );

    let resolveTurn!: (value: unknown) => void;
    queryApiMock.createTurnStream.mockReturnValue(
      new Promise((resolve) => {
        resolveTurn = resolve;
      }),
    );

    await renderPage();

    await waitFor(() => {
      expect(container.textContent).toContain('LLM context was not recorded');
    });

    setTextareaValue('Why does this take time?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
    });

    await waitFor(() => {
      expect(container.textContent).toContain('Agent turn is running');
    });
    expect(container.textContent).not.toContain('LLM context was not recorded');

    resolveTurn({
      responseTurn: {
        id: 'turn-after-debug-clear',
        contentText: 'Done after debug cleared.',
        createdAt: '2026-04-10T11:00:05Z',
        executionId: 'exec-after-debug-clear',
      },
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await waitFor(() => {
      expect(container.textContent).toContain('Done after debug cleared.');
    });
  });

  it('does not open debug context for a pending assistant execution with an id', async () => {
    window.localStorage.setItem(
      'ironrag_assistant_active_session:ws-1:library-1',
      JSON.stringify('session-1'),
    );
    window.localStorage.setItem('ironrag_assistant_debug_open', JSON.stringify(true));
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [
        {
          id: 'msg-user',
          role: 'user',
          content: 'What is still running?',
          timestamp: '2026-04-10T10:00:01Z',
        },
        {
          id: 'msg-assistant-pending',
          role: 'assistant',
          content: '',
          timestamp: '2026-04-10T10:00:05Z',
          executionId: 'exec-running',
        },
      ],
    });

    await renderPage();
    await waitFor(() => {
      expect(container.textContent).toContain('Agent turn is running');
    });
    await flushUi();
    await flushUi();

    expect(queryApiMock.getExecutionLlmContext).not.toHaveBeenCalled();
    expect(container.textContent).not.toContain('LLM context was not recorded');
  });

  it('keeps the active thread fixed while a turn is pending', async () => {
    queryApiMock.listSessions.mockResolvedValue([
      {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      {
        id: 'session-2',
        libraryId: 'library-1',
        title: 'Release notes',
        updatedAt: '2026-04-11T10:00:00Z',
        turnCount: 1,
      },
    ]);
    queryApiMock.getSession.mockImplementation(async (sessionId: string) => ({
      session: {
        id: sessionId,
        libraryId: 'library-1',
        title: sessionId === 'session-1' ? 'Deployment notes' : 'Release notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 1,
      },
      messages: [],
    }));

    let resolveTurn!: (value: unknown) => void;
    queryApiMock.createTurnStream.mockReturnValue(
      new Promise((resolve) => {
        resolveTurn = resolve;
      }),
    );

    await renderPage();

    const deploymentSession = findButton('Deployment notes') as HTMLButtonElement;
    expect(deploymentSession).toBeTruthy();
    await act(async () => {
      deploymentSession.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    setTextareaValue('What is pending?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
    });

    await waitFor(() => {
      expect(queryApiMock.createTurnStream).toHaveBeenCalledWith(
        'session-1',
        'What is pending?',
        0,
        expect.any(Function),
      );
    });

    const releaseSession = findButton('Release notes') as HTMLButtonElement;
    expect(releaseSession.disabled).toBe(true);

    resolveTurn({
      responseTurn: {
        id: 'turn-pending',
        contentText: 'Pending answer landed',
        createdAt: '2026-04-10T11:00:05Z',
        executionId: 'exec-pending',
      },
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await flushUi();
    await flushUi();

    expect(container.textContent).toContain('Pending answer landed');
    expect(visibleTextOccurrences('What is pending?')).toBe(1);
    expect(releaseSession.disabled).toBe(false);
  });

  it('keeps the immediate pending indicator when server hydration lags behind execution creation', async () => {
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 1,
      },
      messages: [],
    });

    let resolveTurn!: (value: unknown) => void;
    queryApiMock.createTurnStream.mockReturnValue(
      new Promise((resolve) => {
        resolveTurn = resolve;
      }),
    );

    const queryClient = await renderPage();

    const deploymentSession = findButton('Deployment notes') as HTMLButtonElement;
    expect(deploymentSession).toBeTruthy();
    await act(async () => {
      deploymentSession.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    setTextareaValue('Why is this slow?');
    await flushUi();

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
    });

    await waitFor(() => {
      expect(container.textContent).toContain('Why is this slow?');
      expect(container.textContent).toContain('Agent turn is running');
    });

    await act(async () => {
      queryClient.setQueryData(['mockedGetQuerySession', 'session-1'], {
        session: {
          id: 'session-1',
          libraryId: 'library-1',
          title: 'Deployment notes',
          updatedAt: '2026-04-10T10:00:01Z',
          turnCount: 1,
        },
        messages: [
          {
            id: 'server-user-1',
            role: 'user',
            content: 'Why is this slow?',
            timestamp: '2026-04-10T10:00:01Z',
          },
        ],
      });
    });
    await flushUi();

    expect(container.textContent).toContain('Why is this slow?');
    expect(container.textContent).toContain('Agent turn is running');

    resolveTurn({
      responseTurn: {
        id: 'turn-slow',
        contentText: 'The request is still visibly running while the provider works.',
        createdAt: '2026-04-10T11:00:05Z',
        executionId: 'exec-slow',
      },
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await waitFor(() => {
      expect(container.textContent).toContain(
        'The request is still visibly running while the provider works.',
      );
    });
  });

  it('resets the selected thread and sends new turns to the current library after a library switch', async () => {
    queryApiMock.listSessions.mockImplementation(async ({ libraryId }) => {
      if (libraryId === 'library-2') {
        return [
          {
            id: 'session-2',
            libraryId: 'library-2',
            title: 'Release notes',
            updatedAt: '2026-04-11T10:00:00Z',
            turnCount: 1,
          },
        ];
      }
      return [
        {
          id: 'session-1',
          libraryId: 'library-1',
          title: 'Deployment notes',
          updatedAt: '2026-04-10T10:00:00Z',
          turnCount: 2,
        },
      ];
    });
    queryApiMock.getSession.mockResolvedValue({
      session: {
        id: 'session-1',
        libraryId: 'library-1',
        title: 'Deployment notes',
        updatedAt: '2026-04-10T10:00:00Z',
        turnCount: 2,
      },
      messages: [
        {
          id: 'msg-assistant',
          role: 'assistant',
          content: 'Library one answer',
          timestamp: '2026-04-10T10:00:02Z',
        },
      ],
    });
    queryApiMock.createSession.mockImplementation(async (_workspaceId, libraryId) => ({
      id: `session-new-${libraryId}`,
      libraryId,
      title: '',
      updatedAt: '2026-04-11T11:00:00Z',
      turnCount: 0,
    }));
    queryApiMock.createTurnStream.mockResolvedValue({
      responseTurn: {
        id: 'turn-2',
        contentText: 'Library two answer',
        createdAt: '2026-04-11T11:00:05Z',
        executionId: 'exec-2',
      },
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeStageSummaries: [],
    });

    await renderPage();

    const sessionButton = findButton('Deployment notes');
    expect(sessionButton).toBeTruthy();
    await act(async () => {
      sessionButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();
    expect(container.textContent).toContain('Library one answer');

    useAppMock.mockReturnValue({
      activeLibrary: {
        id: 'library-2',
        workspaceId: 'ws-1',
        missingBindingPurposes: [],
      },
      activeWorkspace: { id: 'ws-1' },
      locale: 'en',
    });

    await rerenderPage();

    expect(queryApiMock.listSessions).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      libraryId: 'library-2',
    });
    expect(container.textContent).not.toContain('Deployment notes');
    expect(container.textContent).not.toContain('Library one answer');
    expect(container.textContent).toContain('Release notes');

    setTextareaValue('What changed?');
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

    expect(queryApiMock.createSession).toHaveBeenCalledWith('ws-1', 'library-2');
    expect(queryApiMock.createTurnStream).toHaveBeenCalledWith(
      'session-new-library-2',
      'What changed?',
      0,
      expect.any(Function),
    );
    expect(queryApiMock.createTurnStream).not.toHaveBeenCalledWith(
      'session-1',
      expect.any(String),
      expect.any(Number),
      expect.any(Function),
    );
    await waitFor(() => {
      expect(container.textContent).toContain('Library two answer');
    });
  });
});
