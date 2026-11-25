import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { ResponseContractError } from '@/shared/api'
import { Catalog, Ingest, Ops } from '@/shared/api/generated'
import type {
  IngestAttempt,
  IngestAttemptResponse,
  IngestJob,
  IngestJobResponse,
  IngestQueueItemResponse,
  IngestReadinessResponse,
} from '@/shared/api/generated'
import { listIngestQueueQueryKey } from '@/shared/api/generated/@tanstack/react-query.gen'

import { useIngestQueue, useQueueInspector, useQueueLibrary, useQueueMutations } from './api'
import { QueueResponseError } from './model'

const item: IngestQueueItemResponse = {
  availableAt: '2026-08-09T10:00:00Z',
  canCancel: true,
  canPause: false,
  canResume: false,
  canRetryRequeue: true,
  currentStage: 'embed_chunk',
  documentName: 'handbook.pdf',
  hasStaleQueueLease: false,
  jobId: 'job-1',
  jobKind: 'document_ingest',
  libraryId: 'library-1',
  libraryName: 'Handbook',
  progressPercent: 55,
  queuePosition: 1,
  queueState: 'leased',
  queuedAt: '2026-08-09T09:00:00Z',
  workspaceId: 'workspace-1',
  workspaceName: 'Platform',
}

const queue = () => ({
  items: [item],
  summary: { paused: 0, queued: 0, running: 1, total: 1 },
})

const job: IngestJob = {
  available_at: '2026-08-09T10:00:00Z',
  id: 'job-1',
  job_kind: 'document_ingest',
  library_id: 'library-1',
  priority: 0,
  queue_state: 'running',
  queued_at: '2026-08-09T09:00:00Z',
  workspace_id: 'workspace-1',
}
const readiness: IngestReadinessResponse = { graphReady: true, textReady: true, vectorReady: false }
const attempt: IngestAttempt = {
  attempt_number: 1,
  attempt_state: 'running',
  current_stage: 'embed_chunk',
  id: 'attempt-1',
  job_id: 'job-1',
  progress_percent: 55,
  retryable: true,
  started_at: '2026-08-09T09:30:00Z',
}
const jobResponse: IngestJobResponse = { job, readiness }
const attemptResponse: IngestAttemptResponse = { attempt, job, readiness }

let queryClient = new QueryClient()

function freshClient(retry: number | false = false): QueryClient {
  queryClient = new QueryClient({
    defaultOptions: {
      mutations: { retry: false },
      queries: { retry, retryDelay: 0 },
    },
  })
  return queryClient
}

function wrapper({ children }: Readonly<{ children: ReactNode }>) {
  return createElement(QueryClientProvider, { client: queryClient }, children)
}

afterEach(() => {
  queryClient.clear()
  vi.restoreAllMocks()
})

describe('useIngestQueue', () => {
  it('sends the queue scope it was given and returns the validated page', async () => {
    const list = vi.spyOn(Ingest, 'listIngestQueue').mockResolvedValue({ data: queue() } as never)
    freshClient()

    const { result } = renderHook(
      () => useIngestQueue({ libraryId: 'library-1', workspaceId: 'workspace-1' }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data?.summary.total).toBe(1))

    expect(list.mock.calls[0]?.[0]?.query).toEqual({
      libraryId: 'library-1',
      workspaceId: 'workspace-1',
    })
    expect(result.current.data?.items[0]?.documentName).toBe('handbook.pdf')
  })

  it('omits a workspace the scope did not name', async () => {
    const list = vi.spyOn(Ingest, 'listIngestQueue').mockResolvedValue({ data: queue() } as never)
    freshClient()

    const { result } = renderHook(
      () => useIngestQueue({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data?.items).toHaveLength(1))

    expect(list.mock.calls[0]?.[0]?.query).toEqual({ libraryId: 'library-1' })
  })

  it('does not request a queue for an empty explicit library', async () => {
    const list = vi.spyOn(Ingest, 'listIngestQueue').mockResolvedValue({ data: queue() } as never)
    freshClient()

    const { result } = renderHook(() => useIngestQueue({ libraryId: '', workspaceId: undefined }), {
      wrapper,
    })
    await act(async () => {})

    expect(list).not.toHaveBeenCalled()
    expect(result.current.fetchStatus).toBe('idle')
  })

  it('keeps a repeated jobId out of the cache as a contract error', async () => {
    const list = vi
      .spyOn(Ingest, 'listIngestQueue')
      .mockResolvedValue({ data: { items: [item, item], summary: queue().summary } } as never)
    freshClient(3)

    const { result } = renderHook(
      () => useIngestQueue({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.isError).toBe(true))

    expect(result.current.error).toBeInstanceOf(QueueResponseError)
    expect(result.current.error).toBeInstanceOf(ResponseContractError)
    expect(result.current.data).toBeUndefined()
    // A deterministic body is not retried at all.
    expect(list).toHaveBeenCalledTimes(1)
  })

  it('retries one transport failure before settling', async () => {
    const list = vi
      .spyOn(Ingest, 'listIngestQueue')
      .mockRejectedValueOnce(new Error('network down'))
      .mockResolvedValue({ data: queue() } as never)
    freshClient(3)

    const { result } = renderHook(
      () => useIngestQueue({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data?.summary.total).toBe(1))

    expect(list).toHaveBeenCalledTimes(2)
  })

  it('stops the poll on a contract violation and keeps it for a printable queue', async () => {
    vi.spyOn(Ingest, 'listIngestQueue').mockResolvedValue({ data: queue() } as never)
    freshClient()

    const { result } = renderHook(
      () => useIngestQueue({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data?.summary.total).toBe(1))

    const polled = queryClient.getQueryCache().getAll()[0]
    const { refetchInterval } = polled?.options as unknown as {
      refetchInterval: (query: { state: { error: unknown } }) => number | false
    }
    expect(refetchInterval({ state: { error: null } })).toBe(5_000)
    expect(refetchInterval({ state: { error: new ResponseContractError('bad body') } })).toBe(false)
  })
})

describe('useQueueLibrary', () => {
  it('reads the library title for the scope it was given', async () => {
    const get = vi.spyOn(Catalog, 'getCatalogLibrary').mockResolvedValue({
      data: { displayName: 'Handbook', id: 'library-1' },
    } as never)
    freshClient()

    const { result } = renderHook(() => useQueueLibrary('library-1'), { wrapper })
    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    expect(get.mock.calls[0]?.[0]?.path).toEqual({ libraryId: 'library-1' })
  })

  it('stays idle without a library id', async () => {
    const get = vi.spyOn(Catalog, 'getCatalogLibrary')
    freshClient()

    const { result } = renderHook(() => useQueueLibrary(undefined), { wrapper })
    await act(async () => {})

    expect(get).not.toHaveBeenCalled()
    expect(result.current.fetchStatus).toBe('idle')
  })
})

describe('useQueueInspector', () => {
  it('reads the job, its attempts and the selected attempt timeline', async () => {
    const getJob = vi
      .spyOn(Ingest, 'getIngestJob')
      .mockResolvedValue({ data: jobResponse } as never)
    const listAttempts = vi
      .spyOn(Ingest, 'listIngestJobAttempts')
      .mockResolvedValue({ data: { items: [attempt] } } as never)
    const getAttempt = vi
      .spyOn(Ingest, 'getIngestAttempt')
      .mockResolvedValue({ data: attemptResponse } as never)
    const listStages = vi.spyOn(Ingest, 'listIngestStageEvents').mockResolvedValue({
      data: {
        attempt,
        job,
        readiness,
        stages: [
          {
            attempt_id: 'attempt-1',
            details_json: null,
            id: 'c',
            ordinal: 3,
            recorded_at: '',
            stage_name: 'index',
            stage_state: 'completed',
          },
          {
            attempt_id: 'attempt-1',
            details_json: null,
            id: 'a',
            ordinal: 1,
            recorded_at: '',
            stage_name: 'extract',
            stage_state: 'completed',
          },
          {
            attempt_id: 'attempt-1',
            details_json: null,
            id: 'b',
            ordinal: 2,
            recorded_at: '',
            stage_name: 'embed',
            stage_state: 'completed',
          },
        ],
      },
    } as never)
    freshClient()

    const { result } = renderHook(() => useQueueInspector('job-1', 'attempt-1'), { wrapper })
    await waitFor(() => expect(result.current.stagesQuery.data).toHaveLength(3))

    expect(getJob.mock.calls[0]?.[0]?.path).toEqual({ jobId: 'job-1' })
    expect(listAttempts.mock.calls[0]?.[0]?.path).toEqual({ jobId: 'job-1' })
    expect(listAttempts.mock.calls[0]?.[0]?.query).toEqual({ limit: 50 })
    expect(getAttempt.mock.calls[0]?.[0]?.path).toEqual({ attemptId: 'attempt-1' })
    expect(listStages.mock.calls[0]?.[0]?.path).toEqual({ attemptId: 'attempt-1' })
    expect(result.current.resolvedAttemptId).toBe('attempt-1')
    expect(result.current.jobQuery.data?.job.id).toBe('job-1')
    // The timeline reads in the server's own stage order, not in arrival order.
    expect(result.current.stagesQuery.data?.map((event) => event.stage_name)).toEqual([
      'extract',
      'embed',
      'index',
    ])
  })

  it('falls back to the newest attempt when the URL names none', async () => {
    vi.spyOn(Ingest, 'getIngestJob').mockResolvedValue({ data: jobResponse } as never)
    vi.spyOn(Ingest, 'listIngestJobAttempts').mockResolvedValue({
      data: { items: [{ ...attempt, id: 'attempt-2' }] },
    } as never)
    const getAttempt = vi
      .spyOn(Ingest, 'getIngestAttempt')
      .mockResolvedValue({ data: attemptResponse } as never)
    vi.spyOn(Ingest, 'listIngestStageEvents').mockResolvedValue({
      data: { attempt, job, readiness, stages: [] },
    } as never)
    freshClient()

    const { result } = renderHook(() => useQueueInspector('job-1', null), { wrapper })
    await waitFor(() => expect(getAttempt).toHaveBeenCalled())

    expect(result.current.resolvedAttemptId).toBe('attempt-2')
    expect(getAttempt.mock.calls[0]?.[0]?.path).toEqual({ attemptId: 'attempt-2' })
  })

  it('does not read a job or an attempt without a job id', async () => {
    const getJob = vi.spyOn(Ingest, 'getIngestJob')
    const listAttempts = vi.spyOn(Ingest, 'listIngestJobAttempts')
    const getAttempt = vi.spyOn(Ingest, 'getIngestAttempt')
    const listStages = vi.spyOn(Ingest, 'listIngestStageEvents')
    freshClient()

    const { result } = renderHook(() => useQueueInspector(null, null), { wrapper })
    await act(async () => {})

    expect(result.current.resolvedAttemptId).toBeNull()
    expect(getJob).not.toHaveBeenCalled()
    expect(listAttempts).not.toHaveBeenCalled()
    expect(getAttempt).not.toHaveBeenCalled()
    expect(listStages).not.toHaveBeenCalled()
  })
})

describe('useQueueMutations', () => {
  it('sends every queue command as its own request and refreshes the exact queue', async () => {
    const bulk = vi.spyOn(Ops, 'bulkIngestQueueAction').mockResolvedValue({ data: {} } as never)
    const cancel = vi.spyOn(Ops, 'cancelIngestQueueJob').mockResolvedValue({ data: {} } as never)
    const move = vi.spyOn(Ops, 'moveIngestQueueJob').mockResolvedValue({ data: {} } as never)
    const pause = vi.spyOn(Ops, 'pauseIngestQueueJob').mockResolvedValue({ data: {} } as never)
    const resume = vi.spyOn(Ops, 'resumeIngestQueueJob').mockResolvedValue({ data: {} } as never)
    const retry = vi.spyOn(Ops, 'retryIngestQueueJob').mockResolvedValue({ data: {} } as never)
    const client = freshClient()
    const invalidate = vi.spyOn(client, 'invalidateQueries')

    const { result } = renderHook(
      () => useQueueMutations({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )

    await act(async () => {
      result.current.bulk('retry_requeue', ['job-1', 'job-2'])
      result.current.cancel('job-1')
      result.current.move('job-1', 'top')
      result.current.pause('job-1')
      result.current.resume('job-1')
      result.current.retry('job-1')
    })
    await waitFor(() => expect(invalidate).toHaveBeenCalled())

    expect(bulk.mock.calls[0]?.[0]).toMatchObject({
      body: { action: 'retry_requeue', jobIds: ['job-1', 'job-2'] },
    })
    expect(cancel.mock.calls[0]?.[0]).toMatchObject({ path: { jobId: 'job-1' } })
    expect(move.mock.calls[0]?.[0]).toMatchObject({
      body: { direction: 'top' },
      path: { jobId: 'job-1' },
    })
    expect(pause.mock.calls[0]?.[0]).toMatchObject({ path: { jobId: 'job-1' } })
    expect(resume.mock.calls[0]?.[0]).toMatchObject({ path: { jobId: 'job-1' } })
    expect(retry.mock.calls[0]?.[0]).toMatchObject({ path: { jobId: 'job-1' } })
    expect(invalidate).toHaveBeenCalledWith({
      exact: true,
      queryKey: listIngestQueueQueryKey({ query: { libraryId: 'library-1' } }),
    })
  })

  it('keeps the failed command state on the mutation it belongs to', async () => {
    vi.spyOn(Ops, 'pauseIngestQueueJob').mockRejectedValue(new Error('forbidden'))
    freshClient()

    const { result } = renderHook(
      () => useQueueMutations({ libraryId: 'library-1', workspaceId: undefined }),
      { wrapper },
    )
    await act(async () => {
      result.current.pause('job-1')
    })
    await waitFor(() => expect(result.current.pauseState.isError).toBe(true))

    expect(result.current.moveState.isIdle).toBe(true)
    expect(result.current.pauseState.error?.message).toBe('forbidden')
  })
})
