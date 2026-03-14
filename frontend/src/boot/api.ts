import axios from 'axios'

interface FrontendEnv {
  readonly VITE_BACKEND_URL?: string
}

export interface WorkspaceSummary {
  id: string
  slug: string
  name: string
  status?: string
}

export interface CreateWorkspaceRequest {
  slug: string
  name: string
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

export interface ProjectSummary {
  id: string
  workspace_id: string
  slug: string
  name: string
  description?: string | null
}

export interface CreateProjectRequest {
  workspace_id: string
  slug: string
  name: string
  description?: string | null
}

export interface ProviderAccountSummary {
  id: string
  workspace_id: string
  provider_kind: string
  label: string
  status: string
}

export interface CreateProviderAccountRequest {
  workspace_id: string
  provider_kind: string
  label: string
  api_base_url?: string | null
}

export interface ModelProfileSummary {
  id: string
  workspace_id: string
  provider_account_id: string
  profile_kind: string
  model_name: string
}

export interface CreateModelProfileRequest {
  workspace_id: string
  provider_account_id: string
  profile_kind: string
  model_name: string
  temperature?: number | null
  max_output_tokens?: number | null
}

export interface ProviderGovernanceSummary {
  workspace_id: string
  provider_accounts: ProviderAccountSummary[]
  model_profiles: ModelProfileSummary[]
  warning?: string | null
}

export interface SourceSummary {
  id: string
  project_id: string
  source_kind: string
  label: string
  status: string
}

export interface CreateSourceRequest {
  project_id: string
  source_kind: string
  label: string
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

export interface CreateIngestionJobRequest {
  project_id: string
  source_id?: string | null
  trigger_kind: string
  requested_by?: string | null
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

export interface DocumentSummary {
  id: string
  project_id: string
  source_id?: string | null
  external_key: string
  title?: string | null
  mime_type?: string | null
  checksum?: string | null
  status?: string | null
}

export interface ChunkSummary {
  id: string
  document_id: string
  project_id: string
  ordinal: number
  content: string
  token_count?: number | null
}

export interface SearchChunkResult {
  id: string
  document_id: string
  ordinal: number
  content: string
}

export interface SearchChunksRequest {
  project_id: string
  query_text: string
  top_k?: number | null
}

export interface IngestTextRequest {
  project_id: string
  source_id?: string | null
  external_key: string
  title?: string | null
  text: string
}

export interface IngestTextResponse {
  document_id: string
  chunk_count: number
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
export const backendUrl: string = env.VITE_BACKEND_URL?.trim() || window.location.origin

export const api = axios.create({
  baseURL: env.VITE_BACKEND_URL?.trim() || '/v1',
})

export async function fetchWorkspaces(): Promise<WorkspaceSummary[]> {
  const { data } = await api.get<WorkspaceSummary[]>('/v1/workspaces')
  return data
}

export async function createWorkspace(payload: CreateWorkspaceRequest): Promise<WorkspaceSummary> {
  const { data } = await api.post<WorkspaceSummary>('/v1/workspaces', payload)
  return data
}

export async function fetchWorkspaceGovernance(id: string): Promise<WorkspaceGovernanceSummary> {
  const { data } = await api.get<WorkspaceGovernanceSummary>(`/v1/workspaces/${id}/governance`)
  return data
}

export async function fetchProjects(workspaceId?: string): Promise<ProjectSummary[]> {
  const { data } = await api.get<ProjectSummary[]>('/v1/projects', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createProject(payload: CreateProjectRequest): Promise<ProjectSummary> {
  const { data } = await api.post<ProjectSummary>('/v1/projects', payload)
  return data
}

export async function fetchProviderAccounts(workspaceId?: string): Promise<ProviderAccountSummary[]> {
  const { data } = await api.get<ProviderAccountSummary[]>('/v1/provider-accounts', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createProviderAccount(
  payload: CreateProviderAccountRequest,
): Promise<ProviderAccountSummary> {
  const { data } = await api.post<ProviderAccountSummary>('/v1/provider-accounts', payload)
  return data
}

export async function fetchModelProfiles(workspaceId?: string): Promise<ModelProfileSummary[]> {
  const { data } = await api.get<ModelProfileSummary[]>('/v1/model-profiles', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createModelProfile(
  payload: CreateModelProfileRequest,
): Promise<ModelProfileSummary> {
  const { data } = await api.post<ModelProfileSummary>('/v1/model-profiles', payload)
  return data
}

export async function fetchProviderGovernance(id: string): Promise<ProviderGovernanceSummary> {
  const { data } = await api.get<ProviderGovernanceSummary>(`/v1/provider-governance/${id}`)
  return data
}

export async function fetchSources(projectId?: string): Promise<SourceSummary[]> {
  const { data } = await api.get<SourceSummary[]>('/v1/sources', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function createSource(payload: CreateSourceRequest): Promise<SourceSummary> {
  const { data } = await api.post<SourceSummary>('/v1/sources', payload)
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

export async function createIngestionJob(
  payload: CreateIngestionJobRequest,
): Promise<IngestionJobDetail> {
  const { data } = await api.post<IngestionJobDetail>('/v1/ingestion-jobs', payload)
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

export async function fetchDocuments(projectId?: string): Promise<DocumentSummary[]> {
  const { data } = await api.get<DocumentSummary[]>('/v1/documents', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function fetchChunks(options: {
  project_id?: string
  document_id?: string
  limit?: number
}): Promise<ChunkSummary[]> {
  const { data } = await api.get<ChunkSummary[]>('/v1/chunks', {
    params: options,
  })
  return data
}

export async function searchChunks(payload: SearchChunksRequest): Promise<SearchChunkResult[]> {
  const { data } = await api.post<SearchChunkResult[]>('/v1/content/search-chunks', payload)
  return data
}

export async function ingestText(payload: IngestTextRequest): Promise<IngestTextResponse> {
  const { data } = await api.post<IngestTextResponse>('/v1/content/ingest-text', payload)
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
