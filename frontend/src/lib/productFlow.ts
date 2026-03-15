import { computed, type Ref, watch } from 'vue'
import type { RouteLocationNormalizedLoaded, Router } from 'vue-router'

import {
  fetchProjects,
  fetchWorkspaces,
  type ProjectSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import { getSelectedWorkspaceId, setSelectedProjectId } from 'src/stores/flow'
import {
  setWorkspaceWithProjectReset,
  syncWorkspaceProjectScope,
} from 'src/lib/flowSelection'

export type ProductFlowWorkspace = WorkspaceSummary

export type ProductFlowProject = ProjectSummary

export async function hydrateWorkspaceProjectScope(options: {
  setWorkspaces: (items: ProductFlowWorkspace[]) => void
  setProjects: (items: ProductFlowProject[]) => void
}): Promise<{ workspaceId: string; projectId: string }> {
  const workspaces = await fetchWorkspaces()
  options.setWorkspaces(workspaces)

  const workspaceId = getSelectedWorkspaceId() || workspaces[0]?.id || ''

  if (!workspaceId) {
    options.setProjects([])
    setSelectedProjectId('')
    return { workspaceId: '', projectId: '' }
  }

  if (workspaceId !== getSelectedWorkspaceId()) {
    setWorkspaceWithProjectReset(workspaceId)
  }

  const projects = (await fetchProjects(workspaceId))
  options.setProjects(projects)

  return syncWorkspaceProjectScope(workspaces, projects)
}

export function useRouteSyncedSelection(options: {
  route: RouteLocationNormalizedLoaded
  router: Router
  queryKey: string
  availableIds: Ref<readonly string[]>
}) {
  const selectedId = computed<string | null>({
    get: () => {
      const value = options.route.query[options.queryKey]
      return typeof value === 'string' && value.length > 0 ? value : null
    },
    set: (value) => {
      const currentQueryValue = options.route.query[options.queryKey]
      const current = typeof currentQueryValue === 'string' ? currentQueryValue : null

      if ((value ?? null) === current) {
        return
      }

      void options.router.replace({
        query: {
          ...options.route.query,
          [options.queryKey]: value ?? undefined,
        },
      })
    },
  })

  watch(
    options.availableIds,
    (ids) => {
      const currentId = selectedId.value

      if (ids.length === 0) {
        selectedId.value = null
        return
      }

      if (!currentId || !ids.includes(currentId)) {
        selectedId.value = ids[0] ?? null
      }
    },
    { immediate: true },
  )

  return selectedId
}
