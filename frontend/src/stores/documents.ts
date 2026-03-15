import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import {
  api,
  createIngestionJob,
  createSource,
  fetchChunks,
  fetchDocuments,
  fetchIngestionJobs,
  fetchSources,
  ingestText,
  searchChunks,
  type ChunkSummary,
  type CreateIngestionJobRequest,
  type CreateSourceRequest,
  type DocumentSummary,
  type IngestionJobDetail,
  type IngestionJobSummary,
  type IngestTextRequest,
  type SearchChunkResult,
  type SearchChunksRequest,
  type SourceSummary,
} from 'src/boot/api'
import { createAsyncState, type AsyncState } from 'src/types/state'

export interface ProjectDocumentState {
  documents: AsyncState<DocumentSummary[]>
  jobs: AsyncState<IngestionJobSummary[]>
  sources: AsyncState<SourceSummary[]>
}

export const useDocumentsStore = defineStore('documents', () => {
  const byProjectId = ref<Record<string, ProjectDocumentState>>({})
  const jobDetailById = ref<Record<string, AsyncState<IngestionJobDetail | null>>>({})
  const chunksByDocumentId = ref<Record<string, AsyncState<ChunkSummary[]>>>({})
  const chunkSearchByProjectId = ref<
    Record<
      string,
      AsyncState<{
        query: string
        results: SearchChunkResult[]
      }>
    >
  >({})
  const ingestState = ref<AsyncState<{ ingestionJobId: string; status: string; stage: string } | null>>(
    createAsyncState<{ ingestionJobId: string; status: string; stage: string } | null>(null),
  )
  const createSourceState = ref<AsyncState<SourceSummary | null>>(
    createAsyncState<SourceSummary | null>(null),
  )
  const createJobState = ref<AsyncState<IngestionJobSummary | null>>(
    createAsyncState<IngestionJobSummary | null>(null),
  )

  function ensureProjectState(projectId: string): ProjectDocumentState {
    const state =
      byProjectId.value[projectId] ??
      ({
        documents: createAsyncState<DocumentSummary[]>([]),
        jobs: createAsyncState<IngestionJobSummary[]>([]),
        sources: createAsyncState<SourceSummary[]>([]),
      } satisfies ProjectDocumentState)
    byProjectId.value = {
      ...byProjectId.value,
      [projectId]: state,
    }
    return state
  }

  function ensureJobDetailState(jobId: string): AsyncState<IngestionJobDetail | null> {
    const state = jobDetailById.value[jobId] ?? createAsyncState<IngestionJobDetail | null>(null)
    jobDetailById.value = {
      ...jobDetailById.value,
      [jobId]: state,
    }
    return state
  }

  function ensureChunkState(documentId: string): AsyncState<ChunkSummary[]> {
    const state = chunksByDocumentId.value[documentId] ?? createAsyncState<ChunkSummary[]>([])
    chunksByDocumentId.value = {
      ...chunksByDocumentId.value,
      [documentId]: state,
    }
    return state
  }

  function ensureChunkSearchState(projectId: string): AsyncState<{ query: string; results: SearchChunkResult[] }> {
    const state =
      chunkSearchByProjectId.value[projectId] ??
      createAsyncState<{ query: string; results: SearchChunkResult[] }>({
        query: '',
        results: [],
      })
    chunkSearchByProjectId.value = {
      ...chunkSearchByProjectId.value,
      [projectId]: state,
    }
    return state
  }

  const totalDocumentCount = computed(() =>
    Object.values(byProjectId.value).reduce((sum, state) => sum + state.documents.data.length, 0),
  )

  async function fetchProjectJobs(projectId?: string): Promise<IngestionJobSummary[]> {
    const key = projectId ?? '__all__'
    const state = ensureProjectState(key)
    state.jobs.status = 'loading'
    state.jobs.error = null
    try {
      const data = await fetchIngestionJobs(projectId)
      state.jobs.data = data
      state.jobs.status = 'success'
      state.jobs.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.jobs.status = 'error'
      state.jobs.error = error instanceof Error ? error.message : 'Unknown ingestion jobs error'
      throw error
    }
  }

  async function fetchProjectDocuments(projectId: string): Promise<DocumentSummary[]> {
    const state = ensureProjectState(projectId)
    state.documents.status = 'loading'
    state.documents.error = null
    try {
      const data = await fetchDocuments(projectId)
      state.documents.data = data
      state.documents.status = 'success'
      state.documents.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.documents.status = 'error'
      state.documents.error = error instanceof Error ? error.message : 'Unknown documents error'
      throw error
    }
  }

  async function fetchProjectSources(projectId: string): Promise<SourceSummary[]> {
    const state = ensureProjectState(projectId)
    state.sources.status = 'loading'
    state.sources.error = null
    try {
      const data = await fetchSources(projectId)
      state.sources.data = data
      state.sources.status = 'success'
      state.sources.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.sources.status = 'error'
      state.sources.error = error instanceof Error ? error.message : 'Unknown sources error'
      throw error
    }
  }

  async function fetchDocumentChunks(documentId: string, options?: { projectId?: string; limit?: number }) {
    const state = ensureChunkState(documentId)
    state.status = 'loading'
    state.error = null
    try {
      const data = await fetchChunks({
        document_id: documentId,
        project_id: options?.projectId,
        limit: options?.limit,
      })
      state.data = data
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return data
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown chunk inventory error'
      throw error
    }
  }

  async function searchProjectChunks(payload: SearchChunksRequest) {
    const state = ensureChunkSearchState(payload.project_id)
    state.status = 'loading'
    state.error = null
    try {
      const results = await searchChunks(payload)
      state.data = {
        query: payload.query_text,
        results,
      }
      state.status = 'success'
      state.lastLoadedAt = new Date().toISOString()
      return results
    } catch (error) {
      state.status = 'error'
      state.error = error instanceof Error ? error.message : 'Unknown chunk search error'
      throw error
    }
  }

  function clearProjectChunkSearch(projectId: string) {
    const state = ensureChunkSearchState(projectId)
    state.data = {
      query: '',
      results: [],
    }
    state.status = 'idle'
    state.error = null
  }

  async function createSourceForProject(payload: CreateSourceRequest): Promise<SourceSummary> {
    createSourceState.value.status = 'loading'
    createSourceState.value.error = null
    try {
      const created = await createSource(payload)
      const state = ensureProjectState(payload.project_id)
      state.sources.data = [created, ...state.sources.data.filter((item) => item.id !== created.id)]
      state.sources.status = 'success'
      state.sources.lastLoadedAt = new Date().toISOString()
      createSourceState.value.data = created
      createSourceState.value.status = 'success'
      createSourceState.value.lastLoadedAt = new Date().toISOString()
      return created
    } catch (error) {
      createSourceState.value.status = 'error'
      createSourceState.value.error = error instanceof Error ? error.message : 'Unknown source creation error'
      throw error
    }
  }

  async function createJobForProject(payload: CreateIngestionJobRequest): Promise<IngestionJobSummary> {
    createJobState.value.status = 'loading'
    createJobState.value.error = null
    try {
      const created = await createIngestionJob(payload)
      const state = ensureProjectState(payload.project_id)
      state.jobs.data = [created, ...state.jobs.data.filter((item) => item.id !== created.id)]
      state.jobs.status = 'success'
      state.jobs.lastLoadedAt = new Date().toISOString()
      createJobState.value.data = created
      createJobState.value.status = 'success'
      createJobState.value.lastLoadedAt = new Date().toISOString()
      return created
    } catch (error) {
      createJobState.value.status = 'error'
      createJobState.value.error = error instanceof Error ? error.message : 'Unknown ingestion job creation error'
      throw error
    }
  }

  async function ingestTextForProject(
    payload: IngestTextRequest,
  ): Promise<{ ingestionJobId: string; status: string; stage: string }> {
    ingestState.value.status = 'loading'
    ingestState.value.error = null
    try {
      const created = await ingestText(payload)
      ingestState.value.data = {
        ingestionJobId: created.ingestion_job_id,
        status: created.status,
        stage: created.stage,
      }
      ingestState.value.status = 'success'
      ingestState.value.lastLoadedAt = new Date().toISOString()
      return ingestState.value.data
    } catch (error) {
      ingestState.value.status = 'error'
      ingestState.value.error = error instanceof Error ? error.message : 'Unknown document ingest error'
      throw error
    }
  }

  return {
    byProjectId,
    jobDetailById,
    chunksByDocumentId,
    chunkSearchByProjectId,
    totalDocumentCount,
    ingestState,
    createSourceState,
    createJobState,
    ensureProjectState,
    ensureJobDetailState,
    ensureChunkState,
    ensureChunkSearchState,
    fetchProjectJobs,
    fetchProjectDocuments,
    fetchProjectSources,
    fetchDocumentChunks,
    searchProjectChunks,
    clearProjectChunkSearch,
    createSourceForProject,
    createJobForProject,
    ingestTextForProject,
  }
})
