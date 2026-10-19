import type { LibrarySummary, SessionResolveResponse, ShellViewer } from '../../generated/types.gen'

type IamSessionResolveFixture = SessionResolveResponse

const PRINCIPAL_ID = 'principal-1'
const WORKSPACE_ID = 'ws-1'

/**
 * The one owner of the mocked admin viewer. E2E builds its own
 * workspace/library payload but reuses this value, so the role the shell grants
 * has a single definition instead of two that can drift apart.
 */
export const shellViewerFixture: ShellViewer = {
  accessLabel: 'Admin',
  displayName: 'Admin',
  isAdmin: true,
  login: 'admin',
  principalId: PRINCIPAL_ID,
  role: 'admin',
}

const DEFAULT_IAM_SESSION_RESOLVE = {
  bootstrapStatus: { setupRequired: false, ssoAvailable: false },
  locale: 'en',
  me: {
    effectiveGrants: [],
    principal: {
      displayLabel: 'Admin',
      id: PRINCIPAL_ID,
      principalKind: 'user',
      status: 'active',
    },
    user: {
      displayName: 'Admin',
      email: 'admin@example.test',
      login: 'admin',
      principalId: PRINCIPAL_ID,
      role: 'admin',
    },
    workspaceMemberships: [],
  },
  message: null,
  mode: 'authenticated',
  session: {
    expiresAt: '2026-09-01T00:00:00Z',
    sessionId: 'session-1',
    user: {
      displayName: 'Admin',
      email: 'admin@example.test',
      login: 'admin',
      principalId: PRINCIPAL_ID,
    },
  },
  shellBootstrap: {
    activeLibraryId: null,
    activeWorkspaceId: WORKSPACE_ID,
    capabilities: [],
    effectiveGrants: [],
    libraries: [],
    libraryTotalCount: 0,
    locale: 'en',
    viewer: shellViewerFixture,
    warnings: [],
    workspaceMemberships: [],
    workspaces: [
      { id: WORKSPACE_ID, lifecycleState: 'active', name: 'Default workspace', slug: 'default' },
    ],
  },
} satisfies IamSessionResolveFixture

/// A library of the fixture's active workspace, ready enough for the shell to
/// select it — which is what makes a screen's library-scoped query `enabled`.
export function iamSessionResolveLibrary(id: string): LibrarySummary {
  return {
    id,
    ingestionReady: true,
    lifecycleState: 'active',
    missingBindingPurposes: [],
    name: `Library ${id}`,
    queryReady: true,
    slug: `${id}-slug`,
    workspaceId: WORKSPACE_ID,
  }
}

/// The signed-in answer to a session resolve. `libraries` is the one axis the
/// shell branches on — no library means no active scope, so nothing
/// library-scoped can load — and the active library follows the first one given.
export default function iamSessionResolve(
  libraries: LibrarySummary[] = [],
): IamSessionResolveFixture {
  return {
    ...DEFAULT_IAM_SESSION_RESOLVE,
    shellBootstrap: {
      ...DEFAULT_IAM_SESSION_RESOLVE.shellBootstrap,
      activeLibraryId: libraries[0]?.id ?? null,
      libraries,
      libraryTotalCount: libraries.length,
    },
  }
}
