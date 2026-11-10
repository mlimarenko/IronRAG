import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import {
  ResponseContractError,
  queries,
  retryContractViolation,
  validateQueryResponse,
} from '@/shared/api'
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
  const options = queueOptions(scope)
  return useQuery({
    ...options,
    queryFn: validateQueryResponse(options.queryFn, mapQueue),
    enabled: scope.libraryId !== '',
    // A poll that starts without data clears the settled error and renders the
    // screen as loading again. A contract violation cannot repair itself, so it
    // stops the poll and leaves the localized error and its retry action on
    // screen; a transport failure keeps polling for automatic recovery.
    refetchInterval: (query) =>
      query.state.error instanceof ResponseContractError ? false : 5_000,
    retry: retryContractViolation,
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
  const jobOptions = queries.getIngestJobOptions({ path: { jobId: jobId ?? '' } })
  const jobQuery = useQuery({
    ...jobOptions,
    queryFn: validateQueryResponse(jobOptions.queryFn, mapJob),
    enabled: Boolean(jobId),
    retry: retryContractViolation,
    select: mapJob,
  })
  const attemptsOptions = queries.listIngestJobAttemptsOptions({
    path: { jobId: jobId ?? '' },
    query: { limit: 50 },
  })
  const attemptsQuery = useQuery({
    ...attemptsOptions,
    queryFn: validateQueryResponse(attemptsOptions.queryFn, mapAttempts),
    enabled: Boolean(jobId),
    placeholderData: keepPreviousData,
    retry: retryContractViolation,
    select: mapAttempts,
  })
  const resolvedAttemptId = attemptId ?? attemptsQuery.data?.[0]?.id ?? null
  const attemptOptions = queries.getIngestAttemptOptions({
    path: { attemptId: resolvedAttemptId ?? '' },
  })
  const attemptQuery = useQuery({
    ...attemptOptions,
    queryFn: validateQueryResponse(attemptOptions.queryFn, mapAttempt),
    enabled: Boolean(resolvedAttemptId),
    retry: retryContractViolation,
    select: mapAttempt,
  })
  const stageOptions = queries.listIngestStageEventsOptions({
    path: { attemptId: resolvedAttemptId ?? '' },
  })
  const stagesQuery = useQuery({
    ...stageOptions,
    queryFn: validateQueryResponse(stageOptions.queryFn, mapStageEvents),
    enabled: Boolean(resolvedAttemptId),
    refetchInterval: attemptQuery.data?.attempt.attempt_state === 'running' ? 3_000 : false,
    retry: retryContractViolation,
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
