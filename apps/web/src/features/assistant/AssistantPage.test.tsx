import { act } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { waitFor } from '@testing-library/react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import AssistantPage from '@/features/assistant/AssistantPage';

const { useAppMock, queryApiMock, toastErrorMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  toastErrorMock: vi.fn(),
  queryApiMock: {
    listSessions: vi.fn(),
    getSession: vi.fn(),
    createSession: vi.fn(),
    createTurn: vi.fn(),
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

  it('loads the session rail on mount and renders session titles', async () => {
    await renderPage();

    expect(queryApiMock.listSessions).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      libraryId: 'library-1',
    });
    expect(container.textContent).toContain('Deployment notes');
  });

  it('posts a turn and replaces the placeholder with the final answer + evidence', async () => {
    queryApiMock.createTurn.mockResolvedValue({
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

    const sendButton = Array.from(container.querySelectorAll('button')).find(
      (b) => b.getAttribute('disabled') === null && b.querySelector('svg'),
    );
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
    expect(queryApiMock.createTurn).toHaveBeenCalledWith(
      'session-new',
      'Where is the docs page?',
    );
    expect(container.textContent).toContain('Hello world');
  });

  it('retries a transient network reject once, then surfaces the failure without recovering', async () => {
    // Production behaviour: a `Failed to fetch` reject means the request
    // never hit the backend, so AssistantPage retries the turn exactly once.
    // Both attempts fail here, so the user sees the canonical send-error
    // message and never the recovery payload.
    queryApiMock.createTurn.mockRejectedValue(new Error('Failed to fetch'));

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

    expect(queryApiMock.createTurn).toHaveBeenCalledTimes(2);
    expect(container.textContent).not.toContain('Recovered answer');
  });

  it('optimistically shows a first-turn session and rolls back with a toast when session creation fails', async () => {
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

    expect(container.textContent).not.toContain('Will this rollback?');
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
    queryApiMock.createTurn.mockReturnValue(
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
      expect(queryApiMock.createTurn).toHaveBeenCalledWith(
        'session-1',
        'What is pending?',
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
    expect(releaseSession.disabled).toBe(false);
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
    queryApiMock.createTurn.mockResolvedValue({
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
    expect(queryApiMock.createTurn).toHaveBeenCalledWith(
      'session-new-library-2',
      'What changed?',
    );
    expect(queryApiMock.createTurn).not.toHaveBeenCalledWith(
      'session-1',
      expect.any(String),
    );
    await waitFor(() => {
      expect(container.textContent).toContain('Library two answer');
    });
  });
});
