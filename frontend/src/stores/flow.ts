const WORKSPACE_KEY = 'rustrag:selected-workspace-id'
const PROJECT_KEY = 'rustrag:selected-project-id'

interface SelectableItem {
  id: string
}

interface WorkspaceScopedItem extends SelectableItem {
  workspace_id: string
}

export function getSelectedWorkspaceId(): string {
  return window.sessionStorage.getItem(WORKSPACE_KEY) ?? ''
}

export function setSelectedWorkspaceId(id: string): void {
  if (id) {
    window.sessionStorage.setItem(WORKSPACE_KEY, id)
  } else {
    window.sessionStorage.removeItem(WORKSPACE_KEY)
  }
}

export function getSelectedProjectId(): string {
  return window.sessionStorage.getItem(PROJECT_KEY) ?? ''
}

export function setSelectedProjectId(id: string): void {
  if (id) {
    window.sessionStorage.setItem(PROJECT_KEY, id)
  } else {
    window.sessionStorage.removeItem(PROJECT_KEY)
  }
}

export function resetSelectedProjectId(): void {
  window.sessionStorage.removeItem(PROJECT_KEY)
}

function syncSelectedId(
  items: readonly SelectableItem[],
  getSelectedId: () => string,
  setSelectedId: (id: string) => void,
): string {
  const selectedId = getSelectedId()
  if (selectedId && items.some((item) => item.id === selectedId)) {
    return selectedId
  }

  const nextId = items[0]?.id ?? ''
  setSelectedId(nextId)
  return nextId
}

export function syncSelectedWorkspaceId(items: readonly SelectableItem[]): string {
  return syncSelectedId(items, getSelectedWorkspaceId, setSelectedWorkspaceId)
}

export function syncSelectedProjectId(items: readonly SelectableItem[]): string {
  return syncSelectedId(items, getSelectedProjectId, setSelectedProjectId)
}

export function syncWorkspaceProjectScope(
  workspaces: readonly SelectableItem[],
  projects: readonly WorkspaceScopedItem[],
): { workspaceId: string; projectId: string } {
  const workspaceId = syncSelectedWorkspaceId(workspaces)

  if (!workspaceId) {
    setSelectedProjectId('')
    return { workspaceId: '', projectId: '' }
  }

  const scopedProjects = projects.filter((project) => project.workspace_id === workspaceId)
  const currentProjectId = getSelectedProjectId()

  if (currentProjectId && scopedProjects.some((project) => project.id === currentProjectId)) {
    return { workspaceId, projectId: currentProjectId }
  }

  const projectId = syncSelectedProjectId(scopedProjects)
  return { workspaceId, projectId }
}

export function ensureProjectMatchesWorkspace(
  projects: readonly WorkspaceScopedItem[],
  projectId: string,
): string {
  const workspaceId = getSelectedWorkspaceId()

  if (!workspaceId) {
    setSelectedProjectId('')
    return ''
  }

  const selectedProject = projects.find((project) => project.id === projectId)
  if (selectedProject?.workspace_id === workspaceId) {
    return projectId
  }

  const nextProjectId = projects.find((project) => project.workspace_id === workspaceId)?.id ?? ''
  setSelectedProjectId(nextProjectId)
  return nextProjectId
}

export function setWorkspaceWithProjectReset(workspaceId: string): void {
  const previousWorkspaceId = getSelectedWorkspaceId()
  setSelectedWorkspaceId(workspaceId)

  if (!workspaceId || workspaceId !== previousWorkspaceId) {
    setSelectedProjectId('')
  }
}
