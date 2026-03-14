import { ref } from 'vue'
import { defineStore } from 'pinia'

export const useFlowStore = defineStore('flow', () => {
  const workspaceId = ref('')
  const projectId = ref('')

  function selectWorkspace(id: string) {
    workspaceId.value = id
    if (!id) {
      projectId.value = ''
    }
  }

  function selectProject(id: string) {
    projectId.value = id
  }

  function resetProject() {
    projectId.value = ''
  }

  return {
    workspaceId,
    projectId,
    selectWorkspace,
    selectProject,
    resetProject,
  }
})
