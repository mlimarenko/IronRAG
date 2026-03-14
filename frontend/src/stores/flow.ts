import { computed, ref, watch } from 'vue'
import { defineStore } from 'pinia'

import { useProjectsStore } from './projects'
import { useWorkspacesStore } from './workspaces'

export const useFlowStore = defineStore('flow', () => {
  const workspaceId = ref('')
  const projectId = ref('')

  const workspacesStore = useWorkspacesStore()
  const projectsStore = useProjectsStore()

  const selectedWorkspace = computed(
    () => workspacesStore.items.find((item) => item.id === workspaceId.value) ?? null,
  )
  const selectedProject = computed(
    () => projectsStore.items.find((item) => item.id === projectId.value) ?? null,
  )

  watch(workspaceId, async (value, previous) => {
    if (!value) {
      projectId.value = ''
      return
    }

    if (value !== previous) {
      projectId.value = ''
    }

    await projectsStore.fetchList(value)

    if (!projectId.value && projectsStore.items.length > 0) {
      projectId.value = projectsStore.items[0]?.id ?? ''
    }
  })

  async function bootstrap() {
    const workspaces = await workspacesStore.fetchList()

    if (!workspaceId.value && workspaces.length > 0) {
      workspaceId.value = workspaces[0]?.id ?? ''
      return
    }

    if (workspaceId.value) {
      await projectsStore.fetchList(workspaceId.value)
      if (!projectId.value && projectsStore.items.length > 0) {
        projectId.value = projectsStore.items[0]?.id ?? ''
      }
    }
  }

  function selectWorkspace(id: string) {
    workspaceId.value = id
  }

  function selectProject(id: string) {
    projectId.value = id
  }

  return {
    workspaceId,
    projectId,
    selectedWorkspace,
    selectedProject,
    bootstrap,
    selectWorkspace,
    selectProject,
  }
})
