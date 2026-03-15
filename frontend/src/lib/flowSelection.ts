import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  setSelectedWorkspaceId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
} from 'src/stores/flow'

interface WorkspaceScopedItem {
  id: string
  workspace_id: string
}

interface SelectedFlowScope {
  workspaceId: string
  projectId: string
}

interface SelectableItem {
  id: string
}

export function syncWorkspaceProjectScope(
  workspaces: readonly SelectableItem[],
  projects: readonly WorkspaceScopedItem[],
): SelectedFlowScope {
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
