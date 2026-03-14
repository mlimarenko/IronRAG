const WORKSPACE_KEY = 'rustrag:selected-workspace-id'
const PROJECT_KEY = 'rustrag:selected-project-id'

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
