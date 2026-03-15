import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import {
  createWorkspace,
  fetchWorkspaces,
  fetchWorkspaceGovernance,
  type CreateWorkspaceRequest,
  type WorkspaceGovernanceSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import { createAsyncState, type AsyncState } from 'src/types/state'

export interface WorkspaceListItem extends WorkspaceSummary {
  status: string
}

export const useWorkspacesStore = defineStore('workspaces', () => {
  const listState = ref<AsyncState<WorkspaceListItem[]>>(createAsyncState<WorkspaceListItem[]>([]))
  const governanceById = ref<Record<string, AsyncState<WorkspaceGovernanceSummary | null>>>({})
  const createState = ref<AsyncState<WorkspaceListItem | null>>(
    createAsyncState<WorkspaceListItem | null>(null),
  )

  const items = computed(() => listState.value.data)

  function normalizeWorkspace(item: WorkspaceSummary): WorkspaceListItem {
    return {
      ...item,
      status: item.status ?? 'Active',
    }
  }

  function ensureGovernanceState(id: string): AsyncState<WorkspaceGovernanceSummary | null> {
    const state =
      governanceById.value[id] ?? createAsyncState<WorkspaceGovernanceSummary | null>(null)
    governanceById.value = {
      ...governanceById.value,
      [id]: state,
    }
    return state
  }

  async function fetchList(): Promise<WorkspaceListItem[]> {
    listState.value.status = 'loading'
    listState.value.error = null
    try {
      const data = (await fetchWorkspaces()).map(normalizeWorkspace)
      listState.value.data = data
      listState.value.status = 'success'
      listState.value.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      listState.value.status = 'error'
      listState.value.error = error instanceof Error ? error.message : 'Unknown workspace error'
      throw error
    }
  }

  async function createItem(payload: CreateWorkspaceRequest): Promise<WorkspaceListItem> {
    createState.value.status = 'loading'
    createState.value.error = null
    try {
      const created = normalizeWorkspace(await createWorkspace(payload))
      listState.value.data = [
        created,
        ...listState.value.data.filter((item) => item.id !== created.id),
      ]
      listState.value.status = 'success'
      listState.value.lastLoadedAt = new Date().toISOString()
      createState.value.data = created
      createState.value.status = 'success'
      createState.value.lastLoadedAt = new Date().toISOString()
      return created
    } catch (error) {
      createState.value.status = 'error'
      createState.value.error =
        error instanceof Error ? error.message : 'Unknown workspace creation error'
      throw error
    }
  }

  async function fetchGovernance(id: string): Promise<WorkspaceGovernanceSummary> {
    const state = ensureGovernanceState(id)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchWorkspaceGovernance(id)
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown workspace governance error'
      throw error
    }
  }

  return {
    listState,
    governanceById,
    createState,
    items,
    ensureGovernanceState,
    fetchList,
    createItem,
    fetchGovernance,
  }
})
