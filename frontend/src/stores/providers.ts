import { ref } from 'vue'
import { defineStore } from 'pinia'

import {
  createModelProfile,
  createProviderAccount,
  fetchProviderAccounts,
  fetchProviderGovernance,
  fetchModelProfiles,
  type CreateModelProfileRequest,
  type CreateProviderAccountRequest,
  type ModelProfileSummary,
  type ProviderAccountSummary,
  type ProviderGovernanceSummary,
} from 'src/boot/api'
import { createAsyncState, type AsyncState } from 'src/types/state'

export const useProvidersStore = defineStore('providers', () => {
  const governanceByWorkspaceId = ref<Record<string, AsyncState<ProviderGovernanceSummary | null>>>(
    {},
  )
  const accountsByWorkspaceId = ref<Record<string, AsyncState<ProviderAccountSummary[]>>>({})
  const modelProfilesByWorkspaceId = ref<Record<string, AsyncState<ModelProfileSummary[]>>>({})
  const createAccountState = ref<AsyncState<ProviderAccountSummary | null>>(
    createAsyncState<ProviderAccountSummary | null>(null),
  )
  const createModelProfileState = ref<AsyncState<ModelProfileSummary | null>>(
    createAsyncState<ModelProfileSummary | null>(null),
  )

  function ensureGovernanceState(
    workspaceId: string,
  ): AsyncState<ProviderGovernanceSummary | null> {
    const state = governanceByWorkspaceId.value[workspaceId] ?? createAsyncState<ProviderGovernanceSummary | null>(null)
    governanceByWorkspaceId.value = {
      ...governanceByWorkspaceId.value,
      [workspaceId]: state,
    }
    return state
  }

  function ensureAccountsState(workspaceId: string): AsyncState<ProviderAccountSummary[]> {
    const state = accountsByWorkspaceId.value[workspaceId] ?? createAsyncState<ProviderAccountSummary[]>([])
    accountsByWorkspaceId.value = {
      ...accountsByWorkspaceId.value,
      [workspaceId]: state,
    }
    return state
  }

  function ensureModelProfilesState(workspaceId: string): AsyncState<ModelProfileSummary[]> {
    const state = modelProfilesByWorkspaceId.value[workspaceId] ?? createAsyncState<ModelProfileSummary[]>([])
    modelProfilesByWorkspaceId.value = {
      ...modelProfilesByWorkspaceId.value,
      [workspaceId]: state,
    }
    return state
  }

  async function fetchGovernance(workspaceId: string): Promise<ProviderGovernanceSummary> {
    const state = ensureGovernanceState(workspaceId)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchProviderGovernance(workspaceId)
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown provider governance error'
      throw error
    }
  }

  async function fetchAccounts(workspaceId: string): Promise<ProviderAccountSummary[]> {
    const state = ensureAccountsState(workspaceId)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchProviderAccounts(workspaceId)
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown provider accounts error'
      throw error
    }
  }

  async function fetchModelProfilesForWorkspace(workspaceId: string): Promise<ModelProfileSummary[]> {
    const state = ensureModelProfilesState(workspaceId)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchModelProfiles(workspaceId)
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown model profiles error'
      throw error
    }
  }

  async function createAccount(payload: CreateProviderAccountRequest): Promise<ProviderAccountSummary> {
    createAccountState.value.status = 'loading'
    createAccountState.value.error = null
    try {
      const created = await createProviderAccount(payload)
      const accountsState = ensureAccountsState(payload.workspace_id)
      accountsState.data = [created, ...accountsState.data.filter((item) => item.id !== created.id)]
      accountsState.status = 'success'
      accountsState.lastLoadedAt = new Date().toISOString()
      createAccountState.value.data = created
      createAccountState.value.status = 'success'
      createAccountState.value.lastLoadedAt = new Date().toISOString()
      return created
    } catch (error) {
      createAccountState.value.status = 'error'
      createAccountState.value.error = error instanceof Error ? error.message : 'Unknown provider account creation error'
      throw error
    }
  }

  async function createProfile(payload: CreateModelProfileRequest): Promise<ModelProfileSummary> {
    createModelProfileState.value.status = 'loading'
    createModelProfileState.value.error = null
    try {
      const created = await createModelProfile(payload)
      const profilesState = ensureModelProfilesState(payload.workspace_id)
      profilesState.data = [created, ...profilesState.data.filter((item) => item.id !== created.id)]
      profilesState.status = 'success'
      profilesState.lastLoadedAt = new Date().toISOString()
      createModelProfileState.value.data = created
      createModelProfileState.value.status = 'success'
      createModelProfileState.value.lastLoadedAt = new Date().toISOString()
      return created
    } catch (error) {
      createModelProfileState.value.status = 'error'
      createModelProfileState.value.error = error instanceof Error ? error.message : 'Unknown model profile creation error'
      throw error
    }
  }

  return {
    governanceByWorkspaceId,
    accountsByWorkspaceId,
    modelProfilesByWorkspaceId,
    createAccountState,
    createModelProfileState,
    ensureGovernanceState,
    ensureAccountsState,
    ensureModelProfilesState,
    fetchGovernance,
    fetchAccounts,
    fetchModelProfilesForWorkspace,
    createAccount,
    createProfile,
  }
})
