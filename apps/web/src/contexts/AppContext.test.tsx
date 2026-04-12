import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { AppProvider, useApp } from '@/contexts/AppContext';

const { authApiMock } = vi.hoisted(() => ({
  authApiMock: {
    resolveSession: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    bootstrapSetup: vi.fn(),
  },
}));

vi.mock('@/api', () => ({
  authApi: authApiMock,
  ApiError: class MockApiError extends Error {
    status: number;
    body: Record<string, unknown>;

    constructor(status: number, body: Record<string, unknown>) {
      super(String(body?.error ?? body?.message ?? `API error ${status}`));
      this.status = status;
      this.body = body;
    }
  },
}));

function AppContextProbe() {
  const {
    workspaces,
    activeWorkspace,
    libraries,
    activeLibrary,
    setActiveWorkspace,
  } = useApp();

  return (
    <div>
      <div data-testid="active-workspace">{activeWorkspace?.id ?? 'none'}</div>
      <div data-testid="active-library">{activeLibrary?.id ?? 'none'}</div>
      <div data-testid="visible-libraries">{libraries.map((library) => library.id).join(',')}</div>
      {workspaces.map((workspace) => (
        <button key={workspace.id} onClick={() => setActiveWorkspace(workspace)} type="button">
          {workspace.name}
        </button>
      ))}
    </div>
  );
}

function makeSession() {
  return {
    mode: 'authenticated' as const,
    locale: 'en',
    session: { id: 'session-1', expiresAt: '2026-04-12T12:00:00Z' },
    me: {
      principal: { id: 'principal-1', displayLabel: 'Admin User' },
      user: { login: 'admin', displayName: 'Admin User' },
    },
    shellBootstrap: {
      workspaces: [
        { id: 'ws-default', slug: 'default', name: 'Default workspace' },
        { id: 'ws-qg', slug: 'qg', name: 'Quality Gates' },
      ],
      libraries: [
        { id: 'lib-default', workspaceId: 'ws-default', slug: 'default-library', name: 'Default library', ingestionReady: true, missingBindingPurposes: [] },
        { id: 'lib-qg-1', workspaceId: 'ws-qg', slug: 'qg-lib-1', name: 'QG Lib Test', ingestionReady: true, missingBindingPurposes: [] },
        { id: 'lib-qg-2', workspaceId: 'ws-qg', slug: 'qg-lib-2', name: 'Quality Gate 2026-04-12', ingestionReady: true, missingBindingPurposes: [] },
      ],
    },
    bootstrapStatus: { setupRequired: false },
    message: null,
  };
}

describe('AppContext workspace-library scoping', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    localStorage.clear();
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

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  async function renderProbe() {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <AppProvider>
          <AppContextProbe />
        </AppProvider>,
      );
    });

    await flushUi();
    await flushUi();
  }

  it('keeps the active library inside the selected workspace on session restore', async () => {
    localStorage.setItem('ironrag_active_workspace', 'ws-default');
    localStorage.setItem('ironrag_active_library', 'lib-qg-1');
    authApiMock.resolveSession.mockResolvedValue(makeSession());

    await renderProbe();

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-default');
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-default');
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe('lib-default');
  });

  it('clears the active library and filters visible libraries when workspace changes', async () => {
    localStorage.setItem('ironrag_active_workspace', 'ws-qg');
    localStorage.setItem('ironrag_active_library', 'lib-qg-1');
    authApiMock.resolveSession.mockResolvedValue(makeSession());

    await renderProbe();

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg');
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-1');
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe('lib-qg-1,lib-qg-2');

    const defaultWorkspaceButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent === 'Default workspace',
    );
    expect(defaultWorkspaceButton).toBeTruthy();

    await act(async () => {
      defaultWorkspaceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-default');
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('none');
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe('lib-default');
  });
});
