export type ShellResourceKind =
  | 'system'
  | 'workspace'
  | 'library'
  | 'document'
  | 'connector'
  | 'provider_credential'
  | 'library_binding'

export type ShellPermissionKind =
  | 'workspace_admin'
  | 'workspace_read'
  | 'library_read'
  | 'library_write'
  | 'document_read'
  | 'document_write'
  | 'connector_admin'
  | 'credential_admin'
  | 'binding_admin'
  | 'query_run'
  | 'ops_read'
  | 'audit_read'
  | 'iam_admin'

export interface WorkspaceOption {
  id: string
  slug: string
  name: string
  lifecycleState: string
}

export interface LibraryOption {
  id: string
  workspaceId: string
  slug: string
  name: string
  description: string | null
  lifecycleState: string
}

export interface ShellCurrentUser {
  id: string
  email: string
  displayName: string
  initials: string
  principalKind: string
  status: string
  accessLabel: string
}

export interface ShellWorkspaceMembership {
  workspaceId: string
  principalId: string
  membershipState: string
  joinedAt: string
  endedAt: string | null
}

export interface ShellGrant {
  id: string
  principalId: string
  resourceKind: ShellResourceKind
  resourceId: string
  permissionKind: ShellPermissionKind
  grantedAt: string
  expiresAt: string | null
}

export interface ShellCapabilities {
  adminEnabled: boolean
  canCreateWorkspace: boolean
  creatableWorkspaceIds: string[]
}

export interface ShellContextResponse {
  locale: 'en' | 'ru'
  adminEnabled: boolean
  currentUser: ShellCurrentUser
  activeWorkspace: WorkspaceOption
  activeLibrary: LibraryOption
  workspaces: WorkspaceOption[]
  libraries: LibraryOption[]
}
