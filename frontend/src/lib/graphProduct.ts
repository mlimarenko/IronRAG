import { api } from 'src/boot/api'

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

export interface GraphKindCount {
  name: string
  count: number
}

export interface GraphProjectSummaryResponse {
  project_id: string
  coverage: GraphCoverageSummary
  entity_kinds: GraphKindCount[]
  relation_kinds: GraphKindCount[]
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

export async function fetchGraphProductSnapshot(projectId: string): Promise<GraphProductSnapshot> {
  const { data } = await api.get<{ snapshot: GraphProductSnapshot }>(`/graph-products/${projectId}`)
  return data.snapshot
}

export async function fetchGraphProjectSummary(
  projectId: string,
): Promise<GraphProjectSummaryResponse> {
  const { data } = await api.get<GraphProjectSummaryResponse>(`/graph-products/${projectId}/summary`)
  return data
}

export async function searchGraphProduct(
  projectId: string,
  query: string,
  limit = 8,
): Promise<GraphSearchResponse> {
  const { data } = await api.get<GraphSearchResponse>(`/graph-products/${projectId}/search`, {
    params: { q: query, limit },
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

export function isGraphApiUnavailableError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false
  }

  const message = error.message.toLowerCase()
  return (
    message.includes('/graph-products/') &&
    (message.includes('404') || message.includes('405') || message.includes('501'))
  )
}
