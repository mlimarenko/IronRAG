export interface WorkspaceOption {
  id: string
  slug: string
  name: string
}

export interface LibraryOption {
  id: string
  workspaceId: string
  slug: string
  name: string
}

export interface ShellContextResponse {
  locale: 'en' | 'ru'
  adminEnabled: boolean
  currentUser: {
    id: string
    email: string
    displayName: string
    roleLabel: string
    initials: string
  }
  activeWorkspace: WorkspaceOption
  activeLibrary: LibraryOption
  workspaces: WorkspaceOption[]
  libraries: LibraryOption[]
}
