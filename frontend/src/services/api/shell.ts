import type { LibraryOption, ShellContextResponse, WorkspaceOption } from 'src/models/ui/shell'
import { apiHttp, unwrap } from './http'

interface RawWorkspaceOption {
  id: string
  slug: string
  name: string
}

interface RawLibraryOption {
  id: string
  workspace_id: string
  slug: string
  name: string
}

interface RawShellContextResponse {
  locale: 'en' | 'ru'
  admin_enabled: boolean
  current_user: {
    id: string
    email: string
    display_name: string
    role_label: string
    initials: string
  }
  active_workspace: RawWorkspaceOption
  active_library: RawLibraryOption
  workspaces: RawWorkspaceOption[]
  libraries: RawLibraryOption[]
}

function mapWorkspace(item: RawWorkspaceOption): WorkspaceOption {
  return {
    id: item.id,
    slug: item.slug,
    name: item.name,
  }
}

function mapLibrary(item: RawLibraryOption): LibraryOption {
  return {
    id: item.id,
    workspaceId: item.workspace_id,
    slug: item.slug,
    name: item.name,
  }
}

function mapShellContext(item: RawShellContextResponse): ShellContextResponse {
  return {
    locale: item.locale,
    adminEnabled: item.admin_enabled,
    currentUser: {
      id: item.current_user.id,
      email: item.current_user.email,
      displayName: item.current_user.display_name,
      roleLabel: item.current_user.role_label,
      initials: item.current_user.initials,
    },
    activeWorkspace: mapWorkspace(item.active_workspace),
    activeLibrary: mapLibrary(item.active_library),
    workspaces: item.workspaces.map(mapWorkspace),
    libraries: item.libraries.map(mapLibrary),
  }
}

export async function fetchShellContext(): Promise<ShellContextResponse> {
  return mapShellContext(await unwrap(apiHttp.get<RawShellContextResponse>('/ui/context')))
}

export async function updateShellContext(payload: {
  workspaceId?: string
  libraryId?: string
  locale?: 'en' | 'ru'
}): Promise<ShellContextResponse> {
  return mapShellContext(
    await unwrap(
      apiHttp.put<RawShellContextResponse>('/ui/context', {
        workspace_id: payload.workspaceId,
        library_id: payload.libraryId,
        locale: payload.locale,
      }),
    ),
  )
}

export async function createWorkspace(name: string): Promise<WorkspaceOption> {
  return mapWorkspace(
    await unwrap(
      apiHttp.post<RawWorkspaceOption>('/ui/workspaces', {
        name,
      }),
    ),
  )
}

export async function createLibrary(payload: {
  workspaceId: string
  name: string
}): Promise<LibraryOption> {
  return mapLibrary(
    await unwrap(
      apiHttp.post<RawLibraryOption>('/ui/libraries', {
        workspace_id: payload.workspaceId,
        name: payload.name,
      }),
    ),
  )
}
