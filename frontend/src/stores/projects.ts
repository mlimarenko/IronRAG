import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import {
  createProject,
  fetchProjects,
  fetchProjectReadiness,
  type CreateProjectRequest,
  type ProjectReadinessSummary,
  type ProjectSummary,
} from 'src/boot/api'
import { createAsyncState, type AsyncState } from 'src/types/state'

export type ProjectListItem = ProjectSummary

export const useProjectsStore = defineStore('projects', () => {
  const listState = ref<AsyncState<ProjectListItem[]>>(createAsyncState<ProjectListItem[]>([]))
  const readinessById = ref<Record<string, AsyncState<ProjectReadinessSummary | null>>>({})
  const createState = ref<AsyncState<ProjectListItem | null>>(
    createAsyncState<ProjectListItem | null>(null),
  )

  const items = computed(() => listState.value.data)

  function ensureReadinessState(id: string): AsyncState<ProjectReadinessSummary | null> {
    const state = readinessById.value[id] ?? createAsyncState<ProjectReadinessSummary | null>(null)
    readinessById.value = {
      ...readinessById.value,
      [id]: state,
    }
    return state
  }

  async function fetchList(workspaceId?: string): Promise<ProjectListItem[]> {
    listState.value.status = 'loading'
    listState.value.error = null
    try {
      const data = await fetchProjects(workspaceId)
      listState.value.data = data
      listState.value.status = 'success'
      listState.value.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      listState.value.status = 'error'
      listState.value.error = error instanceof Error ? error.message : 'Unknown project error'
      throw error
    }
  }

  async function createItem(payload: CreateProjectRequest): Promise<ProjectListItem> {
    createState.value.status = 'loading'
    createState.value.error = null
    try {
      const created = await createProject(payload)
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
        error instanceof Error ? error.message : 'Unknown project creation error'
      throw error
    }
  }

  async function fetchReadiness(id: string): Promise<ProjectReadinessSummary> {
    const state = ensureReadinessState(id)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchProjectReadiness(id)
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown project readiness error'
      throw error
    }
  }

  return {
    listState,
    readinessById,
    createState,
    items,
    ensureReadinessState,
    fetchList,
    createItem,
    fetchReadiness,
  }
})
