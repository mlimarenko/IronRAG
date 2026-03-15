import axios from 'axios'

import { getApiBearerToken } from 'src/lib/apiAuth'

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

export interface CreateTokenRequest {
  workspace_id?: string | null
  token_kind: string
  label: string
  scopes: string[]
}

export interface BootstrapTokenRequest extends CreateTokenRequest {
  bootstrap_secret?: string | null
}

export interface TokenCreateResponse {
  id: string
  workspace_id?: string | null
  token_kind: string
  label: string
  token: string
  scopes: string[]
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

export interface IngestionJobSummary {
  id: string
  project_id: string
  source_id?: string | null
  trigger_kind: string
  status: string
  stage: string
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
  ingestion_job_id: string
  status: string
  stage: string
}

export interface UploadIngestResponse {
  ingestion_job_id: string
  external_key: string
  status: string
  stage: string
  mime_type?: string | null
  file_kind: 'text_like' | 'pdf' | 'image' | 'binary'
  adapter_status: 'supported_now' | 'planned'
  ingest_mode: string
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

export interface IntegrationsProductSnapshot {
  workspace_id: string
  provider_accounts: ProviderAccountSummary[]
  model_profiles: ModelProfileSummary[]
  projects: ProjectSummary[]
  available_scopes: string[]
  generated_at: string
  warning?: string | null
}

export interface GraphCoverageSummary {
  project_id: string
  entity_count: number
  relation_count: number
  extraction_runs: number
  status: string
  warning?: string | null
}

export interface GraphEntitySummary {
  id: string
  project_id: string
  canonical_name: string
  entity_type?: string | null
  source_chunk_count: number
}

export interface GraphRelationSummary {
  id: string
  project_id: string
  relation_type: string
  from_entity_id: string
  to_entity_id: string
  source_chunk_count: number
}

export interface GraphProductSnapshot {
  project_id: string
  coverage: GraphCoverageSummary
  entities: GraphEntitySummary[]
  relations: GraphRelationSummary[]
  generated_at: string
}

export interface GraphProjectSummaryResponse {
  project_id: string
  coverage: GraphCoverageSummary
  entity_kinds: Array<{ name: string; count: number }>
  relation_kinds: Array<{ name: string; count: number }>
  top_entities: GraphEntitySummary[]
  sample_relations: GraphRelationSummary[]
  generated_at: string
}

export interface GraphEntitySearchHit {
  entity: GraphEntitySummary
  match_reasons: string[]
}

export interface GraphRelationSearchHit {
  relation: GraphRelationSummary
  from_entity_name: string
  to_entity_name: string
  match_reasons: string[]
}

export interface GraphSearchResponse {
  project_id: string
  query: string
  searched_fields: string[]
  result_count: number
  entity_results: GraphEntitySearchHit[]
  relation_results: GraphRelationSearchHit[]
  generated_at: string
  warning?: string | null
}

export interface GraphRelationDetail {
  relation: GraphRelationSummary
  from_entity_name: string
  to_entity_name: string
}

export interface GraphEntityDetailResponse {
  project_id: string
  entity: GraphEntitySummary
  aliases: string[]
  source_document_ids: string[]
  source_chunk_ids: string[]
  observed_relation_count: number
  incoming_relations: GraphRelationDetail[]
  outgoing_relations: GraphRelationDetail[]
  generated_at: string
  warning?: string | null
}

const env = import.meta.env as ImportMetaEnv & FrontendEnv
export const backendUrl: string = env.VITE_BACKEND_URL?.trim() || window.location.origin

export const api = axios.create({
  baseURL: env.VITE_BACKEND_URL?.trim() || '/v1',
})

api.interceptors.request.use((config) => {
  const token = getApiBearerToken()
  if (token) {
    config.headers.set('Authorization', `Bearer ${token}`)
  } else {
    config.headers.delete('Authorization')
  }

  return config
})

api.interceptors.response.use(
  (response) => response,
  (error: unknown) => {
    if (axios.isAxiosError(error)) {
      const status = error.response?.status
      const method = error.config?.method?.toUpperCase() ?? 'REQUEST'
      const url = error.config?.url ?? 'unknown-url'
      const body =
        typeof error.response?.data === 'string'
          ? error.response.data
          : (error.response?.data?.message ?? error.response?.data?.error ?? null)

      const detail = body ? `: ${body}` : ''
      const normalized = new Error(
        `${method} ${url} failed with ${status ?? 'network error'}${detail}`,
      )
      return Promise.reject(normalized)
    }

    return Promise.reject(error)
  },
)

export function isUnauthorizedApiError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false
  }

  const message = error.message.toLowerCase()
  return (
    message.includes('401') || message.includes('unauthorized') || message.includes('authorization')
  )
}

export function isMissingAuthorizationHeaderError(error: unknown): boolean {
  return error instanceof Error && error.message.toLowerCase().includes('missing authorization header')
}

export function isBootstrapNotConfiguredApiError(error: unknown): boolean {
  return error instanceof Error && error.message.toLowerCase().includes('bootstrap token is not configured')
}

export async function fetchWorkspaces(): Promise<WorkspaceSummary[]> {
  const { data } = await api.get<WorkspaceSummary[]>('/workspaces')
  return data
}

export async function createBootstrapToken(
  payload: BootstrapTokenRequest,
): Promise<TokenCreateResponse> {
  const { data } = await api.post<TokenCreateResponse>('/auth/bootstrap-token', payload)
  return data
}

export async function createWorkspace(payload: CreateWorkspaceRequest): Promise<WorkspaceSummary> {
  const { data } = await api.post<WorkspaceSummary>('/workspaces', payload)
  return data
}

export async function fetchWorkspaceGovernance(id: string): Promise<WorkspaceGovernanceSummary> {
  const { data } = await api.get<WorkspaceGovernanceSummary>(`/workspaces/${id}/governance`)
  return data
}

export async function fetchProjects(workspaceId?: string): Promise<ProjectSummary[]> {
  const { data } = await api.get<ProjectSummary[]>('/projects', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createProject(payload: CreateProjectRequest): Promise<ProjectSummary> {
  const { data } = await api.post<ProjectSummary>('/projects', payload)
  return data
}

export async function fetchProviderAccounts(
  workspaceId?: string,
): Promise<ProviderAccountSummary[]> {
  const { data } = await api.get<ProviderAccountSummary[]>('/provider-accounts', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createProviderAccount(
  payload: CreateProviderAccountRequest,
): Promise<ProviderAccountSummary> {
  const { data } = await api.post<ProviderAccountSummary>('/provider-accounts', payload)
  return data
}

export async function fetchModelProfiles(workspaceId?: string): Promise<ModelProfileSummary[]> {
  const { data } = await api.get<ModelProfileSummary[]>('/model-profiles', {
    params: workspaceId ? { workspace_id: workspaceId } : {},
  })
  return data
}

export async function createModelProfile(
  payload: CreateModelProfileRequest,
): Promise<ModelProfileSummary> {
  const { data } = await api.post<ModelProfileSummary>('/model-profiles', payload)
  return data
}

export async function fetchProviderGovernance(id: string): Promise<ProviderGovernanceSummary> {
  const { data } = await api.get<ProviderGovernanceSummary>(`/provider-governance/${id}`)
  return data
}

export async function fetchSources(projectId?: string): Promise<SourceSummary[]> {
  const { data } = await api.get<SourceSummary[]>('/sources', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function createSource(payload: CreateSourceRequest): Promise<SourceSummary> {
  const { data } = await api.post<SourceSummary>('/sources', payload)
  return data
}

export async function fetchUsageSummary(projectId?: string): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/usage-summary', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function fetchIngestionJobDetail(id: string): Promise<IngestionJobDetail> {
  const { data } = await api.get<IngestionJobDetail>(`/ingestion-jobs/${id}`)
  return data
}

export async function fetchIngestionJobs(projectId?: string): Promise<IngestionJobSummary[]> {
  const { data } = await api.get<IngestionJobSummary[]>('/ingestion-jobs', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function createIngestionJob(
  payload: CreateIngestionJobRequest,
): Promise<IngestionJobDetail> {
  const { data } = await api.post<IngestionJobDetail>('/ingestion-jobs', payload)
  return data
}

export async function retryIngestionJob(id: string): Promise<IngestionJobDetail> {
  const { data } = await api.post<IngestionJobDetail>(`/ingestion-jobs/${id}/retry`)
  return data
}

export async function fetchProjectReadiness(id: string): Promise<ProjectReadinessSummary> {
  const { data } = await api.get<ProjectReadinessSummary>(`/projects/${id}/readiness`)
  return data
}

export async function fetchDocuments(projectId?: string): Promise<DocumentSummary[]> {
  const { data } = await api.get<DocumentSummary[]>('/documents', {
    params: projectId ? { project_id: projectId } : {},
  })
  return data
}

export async function fetchChunks(options: {
  project_id?: string
  document_id?: string
  limit?: number
}): Promise<ChunkSummary[]> {
  const { data } = await api.get<ChunkSummary[]>('/chunks', {
    params: options,
  })
  return data
}

export async function searchChunks(payload: SearchChunksRequest): Promise<SearchChunkResult[]> {
  const { data } = await api.post<SearchChunkResult[]>('/content/search-chunks', payload)
  return data
}

export async function ingestText(payload: IngestTextRequest): Promise<IngestTextResponse> {
  const { data } = await api.post<IngestTextResponse>('/content/ingest-text', payload)
  return data
}

export async function uploadAndIngest(payload: {
  project_id: string
  source_id?: string | null
  title?: string | null
  file: File
}): Promise<UploadIngestResponse> {
  const formData = new FormData()
  formData.append('project_id', payload.project_id)
  if (payload.source_id) {
    formData.append('source_id', payload.source_id)
  }
  if (payload.title) {
    formData.append('title', payload.title)
  }
  formData.append('file', payload.file)

  const { data } = await api.post<UploadIngestResponse>('/uploads/ingest', formData)
  return data
}

export async function runQuery(payload: {
  project_id: string
  query_text: string
  model_profile_id?: string
  embedding_model_profile_id?: string
  top_k?: number
}): Promise<QueryResponseSurface> {
  const { data } = await api.post<QueryResponseSurface>('/query', payload)
  return data
}

export async function fetchRetrievalRunDetail(id: string): Promise<RetrievalRunDetail> {
  const { data } = await api.get<RetrievalRunDetail>(`/retrieval-runs/${id}`)
  return data
}

export async function fetchIntegrationsProduct(
  workspaceId: string,
): Promise<IntegrationsProductSnapshot> {
  const { data } = await api.get<{ snapshot: IntegrationsProductSnapshot }>(
    `/integrations-products/${workspaceId}`,
  )
  return data.snapshot
}

export async function fetchGraphProduct(projectId: string): Promise<GraphProductSnapshot> {
  const { data } = await api.get<{ snapshot: GraphProductSnapshot }>(`/graph-products/${projectId}`)
  return data.snapshot
}

export async function fetchGraphSummary(projectId: string): Promise<GraphProjectSummaryResponse> {
  const { data } = await api.get<GraphProjectSummaryResponse>(
    `/graph-products/${projectId}/summary`,
  )
  return data
}

export async function searchGraph(
  projectId: string,
  q: string,
  limit = 8,
): Promise<GraphSearchResponse> {
  const { data } = await api.get<GraphSearchResponse>(`/graph-products/${projectId}/search`, {
    params: { q, limit },
  })
  return data
}

export async function fetchGraphEntityDetail(
  projectId: string,
  entityId: string,
): Promise<GraphEntityDetailResponse> {
  const { data } = await api.get<GraphEntityDetailResponse>(
    `/graph-products/${projectId}/entities/${entityId}`,
  )
  return data
}
