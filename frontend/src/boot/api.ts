import axios from 'axios'

interface FrontendEnv {
  readonly VITE_BACKEND_URL?: string
}

export interface WorkspaceGovernanceSummary {
  id: string
  slug: string
  name: string
  status: string
  projects: number
  provider_accounts: number
  model_profiles: number
  api_tokens: number
  health_state: 'Healthy' | 'Degraded' | 'Unavailable' | 'Misconfigured' | 'Blocked'
  usage: {
    usage_events: number
    prompt_tokens: number
    completion_tokens: number
    total_tokens: number
    estimated_cost: number
  }
}

export interface ProviderGovernanceSummary {
  workspace_id: string
  provider_accounts: {
    id: string
    workspace_id: string
    provider_kind: string
    label: string
    status: string
  }[]
  model_profiles: {
    id: string
    workspace_id: string
    provider_account_id: string
    profile_kind: string
    model_name: string
  }[]
  warning?: string | null
}

export interface UsageSummary {
  project_id?: string | null
  usage_events: number
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  estimated_cost: number
}

const env = import.meta.env as ImportMetaEnv & FrontendEnv
const backendUrl: string = env.VITE_BACKEND_URL ?? 'http://127.0.0.1:8080'

export const api = axios.create({
  baseURL: backendUrl,
})

export async function fetchWorkspaceGovernance(id: string): Promise<WorkspaceGovernanceSummary> {
  const { data } = await api.get<WorkspaceGovernanceSummary>(`/v1/workspaces/${id}/governance`)
  return data
}

export async function fetchProviderGovernance(id: string): Promise<ProviderGovernanceSummary> {
  const { data } = await api.get<ProviderGovernanceSummary>(`/v1/provider-governance/${id}`)
  return data
}

export async function fetchUsageSummary(projectId?: string): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/v1/usage-summary', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}
