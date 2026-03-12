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

export interface IngestionJobDetail {
  id: string
  project_id: string
  source_id?: string | null
  trigger_kind: string
  status: string
  stage: string
  requested_by?: string | null
  error_message?: string | null
  started_at?: string | null
  finished_at?: string | null
  retryable: boolean
  lifecycle:
    | 'Queued'
    | 'Validating'
    | 'Running'
    | 'Partial'
    | 'Completed'
    | 'Failed'
    | 'RetryableFailed'
    | 'Canceled'
}

export interface ProjectReadinessSummary {
  id: string
  workspace_id: string
  slug: string
  name: string
  ingestion_jobs: number
  sources: number
  documents: number
  ready_for_query: boolean
  indexing_state: string
}

export interface QueryResponseSurface {
  retrieval_run_id: string
  project_id: string
  answer: string
  references: string[]
  mode: string
  answer_status: string
  weak_grounding: boolean
  warning?: string | null
}

export interface RetrievalRunDetail {
  id: string
  project_id: string
  query_text: string
  model_profile_id?: string | null
  top_k: number
  response_text?: string | null
  answer_status: string
  weak_grounding: boolean
  references: string[]
  matched_chunk_ids: string[]
  warning?: string | null
  debug_json: Record<string, unknown>
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

export async function fetchIngestionJobDetail(id: string): Promise<IngestionJobDetail> {
  const { data } = await api.get<IngestionJobDetail>(`/v1/ingestion-jobs/${id}`)
  return data
}

export async function retryIngestionJob(id: string): Promise<IngestionJobDetail> {
  const { data } = await api.post<IngestionJobDetail>(`/v1/ingestion-jobs/${id}/retry`)
  return data
}

export async function fetchProjectReadiness(id: string): Promise<ProjectReadinessSummary> {
  const { data } = await api.get<ProjectReadinessSummary>(`/v1/projects/${id}/readiness`)
  return data
}

export async function runQuery(payload: {
  project_id: string
  query_text: string
  model_profile_id?: string
  embedding_model_profile_id?: string
  top_k?: number
}): Promise<QueryResponseSurface> {
  const { data } = await api.post<QueryResponseSurface>('/v1/query', payload)
  return data
}

export async function fetchRetrievalRunDetail(id: string): Promise<RetrievalRunDetail> {
  const { data } = await api.get<RetrievalRunDetail>(`/v1/retrieval-runs/${id}`)
  return data
}
