import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { queries } from '@/shared/api'
import type {
  IngestQueueBulkAction,
  IngestQueueMoveDirection,
  ListIngestQueueData,
} from '@/shared/api/generated'

import { mapAttempt, mapAttempts, mapJob, mapQueue, mapStageEvents } from './model'

export type QueueScope = Readonly<{
  libraryId: string | undefined
  workspaceId: string | undefined
}>

function queueOptions(scope: QueueScope) {
  const query: NonNullable<ListIngestQueueData['query']> = {}
  if (scope.workspaceId) query.workspaceId = scope.workspaceId
  if (scope.libraryId) query.libraryId = scope.libraryId
  return queries.listIngestQueueOptions({ query })
}

export function useIngestQueue(scope: QueueScope) {
  return useQuery({
    ...queueOptions(scope),
    enabled: scope.libraryId !== '',
    refetchInterval: 5_000,
    select: mapQueue,
  })
}

export function useQueueLibrary(libraryId: string | undefined) {
  return useQuery({
    ...queries.getCatalogLibraryOptions({ path: { libraryId: libraryId ?? '' } }),
    enabled: Boolean(libraryId),
  })
}

export function useQueueInspector(jobId: string | null, attemptId: string | null) {
  const jobQuery = useQuery({
    ...queries.getIngestJobOptions({ path: { jobId: jobId ?? '' } }),
    enabled: Boolean(jobId),
    select: mapJob,
  })
  const attemptsQuery = useQuery({
    ...queries.listIngestJobAttemptsOptions({
      path: { jobId: jobId ?? '' },
      query: { limit: 50 },
    }),
    enabled: Boolean(jobId),
    placeholderData: keepPreviousData,
    select: mapAttempts,
  })
  const resolvedAttemptId = attemptId ?? attemptsQuery.data?.[0]?.id ?? null
  const attemptQuery = useQuery({
    ...queries.getIngestAttemptOptions({ path: { attemptId: resolvedAttemptId ?? '' } }),
    enabled: Boolean(resolvedAttemptId),
    select: mapAttempt,
  })
  const stagesQuery = useQuery({
    ...queries.listIngestStageEventsOptions({ path: { attemptId: resolvedAttemptId ?? '' } }),
    enabled: Boolean(resolvedAttemptId),
    refetchInterval: attemptQuery.data?.attempt.attempt_state === 'running' ? 3_000 : false,
    select: mapStageEvents,
  })

  return { attemptQuery, attemptsQuery, jobQuery, resolvedAttemptId, stagesQuery }
}

export function useQueueMutations(scope: QueueScope) {
  const queryClient = useQueryClient()
  const queryKey = queueOptions(scope).queryKey
  const refreshQueue = async () => {
    await queryClient.invalidateQueries({ exact: true, queryKey })
  }

  const move = useMutation({
    ...queries.moveIngestQueueJobMutation(),
    onSuccess: refreshQueue,
  })
  const retry = useMutation({
    ...queries.retryIngestQueueJobMutation(),
    onSuccess: refreshQueue,
  })
  const pause = useMutation({
    ...queries.pauseIngestQueueJobMutation(),
    onSuccess: refreshQueue,
  })
  const resume = useMutation({
    ...queries.resumeIngestQueueJobMutation(),
    onSuccess: refreshQueue,
  })
  const cancel = useMutation({
    ...queries.cancelIngestQueueJobMutation(),
    onSuccess: refreshQueue,
  })
  const bulk = useMutation({
    ...queries.bulkIngestQueueActionMutation(),
    onSuccess: refreshQueue,
  })

  return {
    bulk: (action: IngestQueueBulkAction, jobIds: string[]) =>
      bulk.mutate({ body: { action, jobIds } }),
    bulkState: bulk,
    cancel: (jobId: string) => cancel.mutate({ path: { jobId } }),
    cancelState: cancel,
    move: (jobId: string, direction: IngestQueueMoveDirection) =>
      move.mutate({ body: { direction }, path: { jobId } }),
    moveState: move,
    pause: (jobId: string) => pause.mutate({ path: { jobId } }),
    pauseState: pause,
    resume: (jobId: string) => resume.mutate({ path: { jobId } }),
    resumeState: resume,
    retry: (jobId: string) => retry.mutate({ path: { jobId } }),
    retryState: retry,
  }
}
