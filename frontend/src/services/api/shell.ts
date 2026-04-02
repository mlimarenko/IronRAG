import type {
  LibraryOption,
  ShellCapabilities,
  ShellContextResponse,
  ShellCurrentUser,
  ShellGrant,
  ShellPermissionKind,
  ShellWorkspaceMembership,
  WorkspaceOption,
} from 'src/models/ui/shell'
import { ApiClientError, apiHttp, unwrap } from './http'

interface RawCatalogWorkspace {
  id: string
  slug: string
  displayName: string
  lifecycleState: string
}

interface RawCatalogLibrary {
  id: string
  workspaceId: string
  slug: string
  displayName: string
  description?: string | null
  lifecycleState: string
  ingestionReadiness: {
    ready: boolean
    missingBindingPurposes: LibraryOption['ingestionReadiness']['missingBindingPurposes']
  }
}

interface RawIamPrincipal {
  id: string
  principalKind: string
  status: string
  displayLabel: string
}

interface RawIamUser {
  principalId: string
  email: string
  displayName: string
}

interface RawWorkspaceMembership {
  workspaceId: string
  principalId: string
  membershipState: string
  joinedAt: string
  endedAt?: string | null
}

interface RawGrant {
  id: string
  principalId: string
  resourceKind: string
  resourceId: string
  permissionKind: string
  grantedAt: string
  expiresAt?: string | null
}

interface RawMeResponse {
  principal: RawIamPrincipal
  user?: RawIamUser | null
  workspaceMemberships: RawWorkspaceMembership[]
  effectiveGrants: RawGrant[]
}

export interface ShellBootstrapPayload {
  currentUser: ShellCurrentUser
  workspaces: WorkspaceOption[]
  activeWorkspace: WorkspaceOption | null
  libraries: LibraryOption[]
  activeLibrary: LibraryOption | null
  workspaceMemberships: ShellWorkspaceMembership[]
  effectiveGrants: ShellGrant[]
  capabilities: ShellCapabilities
}

function mapWorkspace(item: RawCatalogWorkspace): WorkspaceOption {
  return {
    id: item.id,
    slug: item.slug,
    name: item.displayName,
    lifecycleState: item.lifecycleState,
  }
}

function mapLibrary(item: RawCatalogLibrary): LibraryOption {
  return {
    id: item.id,
    workspaceId: item.workspaceId,
    slug: item.slug,
    name: item.displayName,
    description: item.description ?? null,
    lifecycleState: item.lifecycleState,
    ingestionReadiness: {
      ready: item.ingestionReadiness.ready,
      missingBindingPurposes: item.ingestionReadiness.missingBindingPurposes,
    },
  }
}

function mapWorkspaceMembership(item: RawWorkspaceMembership): ShellWorkspaceMembership {
  return {
    workspaceId: item.workspaceId,
    principalId: item.principalId,
    membershipState: item.membershipState,
    joinedAt: item.joinedAt,
    endedAt: item.endedAt ?? null,
  }
}

function mapGrant(item: RawGrant): ShellGrant {
  return {
    id: item.id,
    principalId: item.principalId,
    resourceKind: item.resourceKind as ShellGrant['resourceKind'],
    resourceId: item.resourceId,
    permissionKind: item.permissionKind as ShellPermissionKind,
    grantedAt: item.grantedAt,
    expiresAt: item.expiresAt ?? null,
  }
}

function deriveInitials(displayName: string, fallbackEmail: string): string {
  const parts = displayName
    .split(/\s+/)
    .map((part) => part.trim())
    .filter(Boolean)
    .slice(0, 2)

  if (parts.length > 0) {
    return parts.map((part) => part.charAt(0).toUpperCase()).join('')
  }

  return fallbackEmail.charAt(0).toUpperCase() || '?'
}

function buildAccessLabel(grants: ShellGrant[], capabilities: ShellCapabilities): string {
  if (capabilities.adminEnabled) {
    return 'Admin access'
  }

  const hasWriteGrant = grants.some((grant) =>
    [
      'workspace_admin',
      'library_write',
      'document_write',
      'connector_admin',
      'credential_admin',
      'binding_admin',
      'iam_admin',
    ].includes(grant.permissionKind),
  )
  if (
    hasWriteGrant ||
    capabilities.canCreateWorkspace ||
    capabilities.creatableWorkspaceIds.length > 0
  ) {
    return 'Write access'
  }

  return 'Read access'
}

function mapCurrentUser(
  me: RawMeResponse,
  capabilities: ShellCapabilities,
  grants: ShellGrant[],
): ShellCurrentUser {
  const email = me.user?.email ?? `${me.principal.displayLabel}@local`
  const displayName = me.user?.displayName ?? me.principal.displayLabel

  return {
    id: me.principal.id,
    email,
    displayName,
    initials: deriveInitials(displayName, email),
    principalKind: me.principal.principalKind,
    status: me.principal.status,
    accessLabel: buildAccessLabel(grants, capabilities),
  }
}

function normalizeWorkspaceSelection(
  workspaces: WorkspaceOption[],
  preferredWorkspaceId?: string | null,
): WorkspaceOption | null {
  if (workspaces.length === 0) {
    return null
  }

  const selectedWorkspace = workspaces.find((workspace) => workspace.id === preferredWorkspaceId)
  return selectedWorkspace ?? workspaces[0]
}

function normalizeLibrarySelection(
  libraries: LibraryOption[],
  preferredLibraryId?: string | null,
): LibraryOption | null {
  if (libraries.length === 0) {
    return null
  }

  const selectedLibrary = libraries.find((library) => library.id === preferredLibraryId)
  return selectedLibrary ?? libraries[0]
}

function deriveCapabilities(
  grants: ShellGrant[],
  memberships: ShellWorkspaceMembership[],
): ShellCapabilities {
  const hasSystemIamAdmin = grants.some(
    (grant) => grant.resourceKind === 'system' && grant.permissionKind === 'iam_admin',
  )
  const creatableWorkspaceIds = new Set<string>()

  for (const grant of grants) {
    if (grant.resourceKind === 'workspace' && grant.permissionKind === 'workspace_admin') {
      creatableWorkspaceIds.add(grant.resourceId)
    }
  }

  for (const membership of memberships) {
    if (membership.membershipState === 'active' && hasSystemIamAdmin) {
      creatableWorkspaceIds.add(membership.workspaceId)
    }
  }

  const adminEnabled =
    hasSystemIamAdmin ||
    grants.some((grant) =>
      ['workspace_admin', 'credential_admin', 'binding_admin', 'ops_read', 'audit_read'].includes(
        grant.permissionKind,
      ),
    )

  return {
    adminEnabled,
    canCreateWorkspace: hasSystemIamAdmin,
    creatableWorkspaceIds: Array.from(creatableWorkspaceIds),
  }
}

async function fetchMe(): Promise<RawMeResponse> {
  return unwrap(apiHttp.get<RawMeResponse>('/iam/me'))
}

async function fetchWorkspaces(): Promise<WorkspaceOption[]> {
  const items = await unwrap(apiHttp.get<RawCatalogWorkspace[]>('/catalog/workspaces'))
  return items.map(mapWorkspace)
}

export async function fetchLibrariesForWorkspace(workspaceId: string): Promise<LibraryOption[]> {
  const items = await unwrap(
    apiHttp.get<RawCatalogLibrary[]>(`/catalog/workspaces/${workspaceId}/libraries`),
  )
  return items.map(mapLibrary)
}

export async function fetchShellBootstrap(
  payload: {
    preferredWorkspaceId?: string | null
    preferredLibraryId?: string | null
  } = {},
): Promise<ShellBootstrapPayload> {
  const [me, workspaces] = await Promise.all([fetchMe(), fetchWorkspaces()])
  const workspaceMemberships = me.workspaceMemberships.map(mapWorkspaceMembership)
  const effectiveGrants = me.effectiveGrants.map(mapGrant)
  const capabilities = deriveCapabilities(effectiveGrants, workspaceMemberships)
  const currentUser = mapCurrentUser(me, capabilities, effectiveGrants)
  const activeWorkspace = normalizeWorkspaceSelection(workspaces, payload.preferredWorkspaceId)
  const libraries = activeWorkspace ? await fetchLibrariesForWorkspace(activeWorkspace.id) : []
  const activeLibrary = normalizeLibrarySelection(libraries, payload.preferredLibraryId)

  return {
    currentUser,
    workspaces,
    activeWorkspace,
    libraries,
    activeLibrary,
    workspaceMemberships,
    effectiveGrants,
    capabilities,
  }
}

export function buildShellContext(
  payload: ShellBootstrapPayload,
  locale: 'en' | 'ru',
): ShellContextResponse | null {
  if (!payload.activeWorkspace || !payload.activeLibrary) {
    return null
  }

  return {
    locale,
    adminEnabled: payload.capabilities.adminEnabled,
    currentUser: payload.currentUser,
    activeWorkspace: payload.activeWorkspace,
    activeLibrary: payload.activeLibrary,
    workspaces: payload.workspaces,
    libraries: payload.libraries,
  }
}

function normalizeMissingCatalogCreateEndpoint(error: unknown): never {
  if (error instanceof ApiClientError && (error.statusCode === 404 || error.statusCode === 405)) {
    throw new Error('Canonical catalog create endpoint is not available on the backend yet')
  }
  throw error
}

export async function createWorkspace(name: string): Promise<WorkspaceOption> {
  try {
    return mapWorkspace(
      await unwrap(
        apiHttp.post<RawCatalogWorkspace>('/catalog/workspaces', {
          displayName: name,
        }),
      ),
    )
  } catch (error) {
    normalizeMissingCatalogCreateEndpoint(error)
  }
}

export async function createLibrary(payload: {
  workspaceId: string
  name: string
}): Promise<LibraryOption> {
  try {
    return mapLibrary(
      await unwrap(
        apiHttp.post<RawCatalogLibrary>(`/catalog/workspaces/${payload.workspaceId}/libraries`, {
          displayName: payload.name,
        }),
      ),
    )
  } catch (error) {
    normalizeMissingCatalogCreateEndpoint(error)
  }
}

export async function deleteWorkspace(workspaceId: string): Promise<void> {
  try {
    await unwrap(apiHttp.delete(`/catalog/workspaces/${workspaceId}`))
  } catch (error) {
    normalizeMissingCatalogCreateEndpoint(error)
  }
}

export async function deleteLibrary(workspaceId: string, libraryId: string): Promise<void> {
  try {
    await unwrap(apiHttp.delete(`/catalog/workspaces/${workspaceId}/libraries/${libraryId}`))
  } catch (error) {
    normalizeMissingCatalogCreateEndpoint(error)
  }
}
