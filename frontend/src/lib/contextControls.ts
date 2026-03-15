import {
  fetchProjects,
  fetchWorkspaces,
  isUnauthorizedApiError,
  type ProjectSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import { getSelectedProjectId, getSelectedWorkspaceId, setSelectedProjectId } from 'src/stores/flow'
import { setWorkspaceWithProjectReset } from 'src/lib/flowSelection'

export interface ContextControlsState {
  workspaces: WorkspaceSummary[]
  projects: ProjectSummary[]
  selectedWorkspaceId: string
  selectedProjectId: string
  authBlocked?: boolean
}

export type ContextControlsStatus =
  | 'loading'
  | 'ready'
  | 'empty'
  | 'error'
  | 'workspace_only'
  | 'auth_required'

export interface ContextControlsPresentation {
  status: ContextControlsStatus
  hasContext: boolean
  hasWorkspaceChoices: boolean
  hasProjectChoices: boolean
  hasWorkspaces: boolean
  hasProjects: boolean
  selectedWorkspace: WorkspaceSummary | null
  selectedProject: ProjectSummary | null
  showAdvancedActions: boolean
  authBlocked: boolean
}

export function buildContextControlsPresentation(
  state: ContextControlsState,
): ContextControlsPresentation {
  const matchingWorkspace = state.workspaces.find((item) => item.id === state.selectedWorkspaceId)
  const matchingProject = state.projects.find((item) => item.id === state.selectedProjectId)
  const selectedWorkspace = matchingWorkspace ?? null
  const selectedProject = matchingProject ?? null
  const hasWorkspaces = state.workspaces.length > 0
  const hasProjects = state.projects.length > 0
  const hasContext = Boolean(selectedWorkspace && selectedProject)
  const authBlocked = Boolean(state.authBlocked)

  let status: ContextControlsStatus = 'ready'
  if (authBlocked) {
    status = 'auth_required'
  } else if (!hasWorkspaces) {
    status = 'empty'
  } else if (!hasProjects || !selectedProject) {
    status = 'workspace_only'
  } else if (!hasContext) {
    status = 'empty'
  }

  return {
    status,
    hasContext,
    hasWorkspaceChoices: state.workspaces.length > 1,
    hasProjectChoices: state.projects.length > 1,
    hasWorkspaces,
    hasProjects,
    selectedWorkspace,
    selectedProject,
    showAdvancedActions:
      authBlocked || !hasContext || state.workspaces.length > 1 || state.projects.length > 1,
    authBlocked,
  }
}

export async function hydrateContextControlsState(): Promise<ContextControlsState> {
  try {
    const workspaces = await fetchWorkspaces()

    const matchingWorkspace = workspaces.find((item) => item.id === getSelectedWorkspaceId())
    const selectedWorkspaceId = matchingWorkspace?.id ?? workspaces.at(0)?.id ?? ''

    if (selectedWorkspaceId !== getSelectedWorkspaceId()) {
      setWorkspaceWithProjectReset(selectedWorkspaceId)
    }

    if (!selectedWorkspaceId) {
      setSelectedProjectId('')
      return {
        workspaces,
        projects: [],
        selectedWorkspaceId: '',
        selectedProjectId: '',
        authBlocked: false,
      }
    }

    const projects = await fetchProjects(selectedWorkspaceId)
    const storedProjectId = getSelectedProjectId()
    const matchingProject = projects.find((item) => item.id === storedProjectId)
    const selectedProjectId = matchingProject ? matchingProject.id : (projects[0]?.id ?? '')

    setSelectedProjectId(selectedProjectId)

    return {
      workspaces,
      projects,
      selectedWorkspaceId,
      selectedProjectId,
      authBlocked: false,
    }
  } catch (error) {
    if (isUnauthorizedApiError(error)) {
      setSelectedProjectId('')
      return {
        workspaces: [],
        projects: [],
        selectedWorkspaceId: '',
        selectedProjectId: '',
        authBlocked: true,
      }
    }

    throw error
  }
}

export async function switchContextWorkspace(workspaceId: string): Promise<ContextControlsState> {
  setWorkspaceWithProjectReset(workspaceId)

  if (!workspaceId) {
    return {
      workspaces: [],
      projects: [],
      selectedWorkspaceId: '',
      selectedProjectId: '',
      authBlocked: false,
    }
  }

  const [workspaces, projects] = await Promise.all([fetchWorkspaces(), fetchProjects(workspaceId)])
  const selectedProjectId = projects[0]?.id ?? ''
  setSelectedProjectId(selectedProjectId)

  return {
    workspaces,
    projects,
    selectedWorkspaceId: workspaceId,
    selectedProjectId,
    authBlocked: false,
  }
}
