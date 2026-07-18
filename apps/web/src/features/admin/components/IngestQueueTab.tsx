import { Fragment, useCallback, useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import {
  ArrowDown,
  ArrowUp,
  Boxes,
  Building2,
  CheckSquare,
  Clock3,
  ExternalLink,
  ListFilter,
  Pause,
  Play,
  RefreshCw,
  Search,
  Square,
  Wrench,
} from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { adminApi, queries } from '@/shared/api'
import type {
  BulkIngestQueueActionResponse,
  IngestQueueBulkAction,
  IngestQueueItemResponse,
  IngestQueueMoveDirection,
  IngestQueueResponse,
  IngestStageEvent,
} from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { DataState } from '@/shared/components/DataState'
import { FilterSelect } from '@/shared/components/FilterSelect'
import { DataWorkspaceView } from '@/shared/components/layout/DataView'
import { InspectorPanel } from '@/shared/components/layout/InspectorPanel'
import { RowActionsMenu, type RowAction } from '@/shared/components/layout/RowActionsMenu'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import { StatusBadge, type StatusTone } from '@/shared/components/StatusBadge'
import { TablePaginationFooter } from '@/shared/components/TablePaginationFooter'
import {
  TABLE_PAGE_SIZE_OPTIONS,
  type TablePageSizeOption,
} from '@/shared/components/tablePagination'
import { Button } from '@/shared/components/ui/button'
import { Checkbox } from '@/shared/components/ui/checkbox'
import { Input } from '@/shared/components/ui/input'
import { SelectItem } from '@/shared/components/ui/select'
import { errorMessage } from '@/shared/lib/errorMessage'
import { buildDocumentFailureNotice, humanizeDocumentStage } from '@/shared/lib/document-processing'
import { isStorageRecord, parseNumberOption, useTableState } from '@/shared/hooks/useTableState'

type QueueStateFilter = 'active' | 'running' | 'queued' | 'paused'
type QueuePageSize = TablePageSizeOption
type QueueScopeOption = {
  id: string
  label: string
  count: number
}

const ALL_SCOPE_VALUE = 'all'

type QueueTableState = {
  pageSize: QueuePageSize
}

const DEFAULT_QUEUE_TABLE_STATE: QueueTableState = {
  pageSize: 50,
}

type IngestQueueTabProps = Readonly<{
  t: TFunction
  active: boolean
}>

type QueueTimelineQuery = Readonly<{
  isLoading: boolean
  error: unknown
  data?: { stages?: IngestStageEvent[] }
}>

function formatQueueTime(value?: string | null): string {
  if (!value) return '—'
  return new Date(value).toLocaleString()
}

function stageLabel(item: IngestQueueItemResponse, t: TFunction): string {
  if (isPausing(item)) {
    return t('admin.queueStatePausing')
  }
  if (item.queueState === 'paused') {
    return t('admin.queueStatePaused')
  }
  if (item.queueState === 'queued') {
    return t('admin.queueStateQueued')
  }
  return humanizeDocumentStage(item.currentStage, t) ?? t('admin.queueStateRunning')
}

function stateTone(queueState: string): StatusTone {
  if (queueState === 'leased') return 'processing'
  if (queueState === 'paused') return 'warning'
  return 'queued'
}

function stateLabel(item: IngestQueueItemResponse, t: TFunction): string {
  if (isPausing(item)) return t('admin.queueStatePausing')
  if (item.queueState === 'leased') return t('admin.queueStateRunning')
  if (item.queueState === 'paused') return t('admin.queueStatePaused')
  return t('admin.queueStateQueued')
}

function isPausing(item: IngestQueueItemResponse): boolean {
  return (
    item.queueState === 'paused' &&
    (item.attemptState === 'leased' || item.attemptState === 'running')
  )
}

function canMove(item: IngestQueueItemResponse): boolean {
  return item.queueState === 'queued' || item.queueState === 'paused'
}

function canPause(item: IngestQueueItemResponse): boolean {
  return item.canPause
}

function canResume(item: IngestQueueItemResponse): boolean {
  return item.canResume
}

function canRetryRequeue(item: IngestQueueItemResponse): boolean {
  return item.canRetryRequeue
}

function canCancel(item: IngestQueueItemResponse): boolean {
  return item.canCancel
}

function progressValue(item: IngestQueueItemResponse): number {
  return Math.max(0, Math.min(100, item.progressPercent ?? (item.queueState === 'queued' ? 0 : 1)))
}

function eventTone(event: IngestStageEvent): string {
  if (event.stage_state === 'failed') return 'bg-status-failed'
  if (event.stage_state === 'completed') return 'bg-status-ready'
  if (event.stage_state === 'running' || event.stage_state === 'started')
    return 'bg-status-processing'
  return 'bg-muted-foreground/35'
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function formatDetailValue(value: unknown): string {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return '—'
  return JSON.stringify(value)
}

function queueProgressLabel(item: IngestQueueItemResponse, t: TFunction): string {
  if (isPausing(item)) return t('admin.queuePausingWaiting')
  if (item.queueState === 'paused') return t('admin.queuePausedWaiting')
  if (item.progressPercent != null) {
    return t('admin.queueProgressValue', { value: item.progressPercent })
  }
  if (item.attemptNumber) {
    return t('admin.queueAttemptValue', { value: item.attemptNumber })
  }
  return t('admin.queueWaiting')
}

function queueRowClassName(selected: boolean, hasFailure: boolean): string {
  if (selected) return 'border-l-2 border-l-primary bg-primary/5'
  if (hasFailure) {
    return 'border-l-2 border-l-destructive/60 bg-destructive/[0.03] hover:bg-destructive/[0.06]'
  }
  return 'hover:bg-accent/30'
}

function stageDetails(event: IngestStageEvent): Array<[string, string]> {
  if (!isRecord(event.details_json)) return []
  return Object.entries(event.details_json)
    .filter(([, value]) => value !== null && value !== undefined && value !== '')
    .slice(0, 6)
    .map(([key, value]) => [key, formatDetailValue(value)])
}

function queueTimelineContent(
  selectedItem: IngestQueueItemResponse,
  timelineQuery: QueueTimelineQuery,
  t: TFunction,
) {
  if (!selectedItem.attemptId) {
    return (
      <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
        {t('admin.queueInspectorNoAttempt')}
      </div>
    )
  }
  if (timelineQuery.isLoading) {
    return (
      <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
        {t('admin.queueInspectorLoading')}
      </div>
    )
  }
  if (timelineQuery.error) {
    return (
      <div className="inline-error text-destructive">
        {errorMessage(timelineQuery.error, t('admin.queueInspectorError'))}
      </div>
    )
  }
  const stages = timelineQuery.data?.stages ?? []
  if (stages.length === 0) {
    return (
      <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
        {t('admin.queueInspectorNoEvents')}
      </div>
    )
  }
  return (
    <div className="overflow-hidden rounded-lg border">
      {stages.map((event) => {
        const details = stageDetails(event)
        return (
          <div key={event.id} className="border-b p-3 last:border-b-0">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${eventTone(event)}`} />
                  <span className="truncate text-sm font-semibold">{event.stage_name}</span>
                </div>
                {event.message && (
                  <p className="mt-1 whitespace-pre-wrap text-xs text-muted-foreground">
                    {event.message}
                  </p>
                )}
              </div>
              <div className="shrink-0 text-right text-2xs text-muted-foreground">
                <div>{event.stage_state}</div>
                <div>{formatQueueTime(event.recorded_at)}</div>
              </div>
            </div>
            {details.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5">
                {details.map(([key, value]) => (
                  <span key={key} className="rounded-md bg-muted px-2 py-1 text-2xs">
                    <span className="text-muted-foreground">{key}</span>{' '}
                    <span className="font-semibold">{value}</span>
                  </span>
                ))}
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}

function queueInspectorContent(
  selectedItem: IngestQueueItemResponse | null,
  timelineQuery: QueueTimelineQuery,
  t: TFunction,
) {
  if (!selectedItem) return null
  return (
    <>
      <div>
        <div className="mb-1 flex items-center justify-between text-xs">
          <span className="font-semibold">{t('admin.queueInspectorProgress')}</span>
          <span className="font-mono">{progressValue(selectedItem)}%</span>
        </div>
        <div className="h-2 rounded-full bg-muted">
          <div
            className="h-full rounded-full bg-primary transition-all"
            style={{ width: `${progressValue(selectedItem)}%` }}
          />
        </div>
      </div>
      <QueueFailureNotice item={selectedItem} t={t} />
      <div>
        <div className="mb-2 flex items-center gap-2 section-label">
          <Clock3 className="h-3.5 w-3.5" />
          {t('admin.queueInspectorTimeline')}
        </div>
        {queueTimelineContent(selectedItem, timelineQuery, t)}
      </div>
    </>
  )
}

function parseQueueTableState(raw: unknown): QueueTableState {
  if (!isStorageRecord(raw)) return DEFAULT_QUEUE_TABLE_STATE
  return {
    pageSize: parseNumberOption(
      raw.pageSize,
      TABLE_PAGE_SIZE_OPTIONS,
      DEFAULT_QUEUE_TABLE_STATE.pageSize,
    ),
  }
}

function sortScopeOptions(options: QueueScopeOption[]): QueueScopeOption[] {
  return options.toSorted((first, second) =>
    first.label.localeCompare(second.label, undefined, {
      numeric: true,
      sensitivity: 'base',
    }),
  )
}

function resolveScopeFilter(selected: string, options: QueueScopeOption[]): string {
  if (selected === ALL_SCOPE_VALUE) return selected
  return options.some((option) => option.id === selected) ? selected : ALL_SCOPE_VALUE
}

function buildWorkspaceOptions(items: IngestQueueItemResponse[]): QueueScopeOption[] {
  const byId = new Map<string, QueueScopeOption>()
  for (const item of items) {
    const current = byId.get(item.workspaceId)
    byId.set(item.workspaceId, {
      id: item.workspaceId,
      label: item.workspaceName,
      count: (current?.count ?? 0) + 1,
    })
  }
  return sortScopeOptions(Array.from(byId.values()))
}

function buildLibraryOptions(
  items: IngestQueueItemResponse[],
  workspaceFilter: string,
): QueueScopeOption[] {
  const byId = new Map<string, QueueScopeOption>()
  for (const item of items) {
    if (workspaceFilter !== ALL_SCOPE_VALUE && item.workspaceId !== workspaceFilter) continue
    const current = byId.get(item.libraryId)
    const label =
      workspaceFilter === ALL_SCOPE_VALUE
        ? `${item.libraryName} · ${item.workspaceName}`
        : item.libraryName
    byId.set(item.libraryId, {
      id: item.libraryId,
      label,
      count: (current?.count ?? 0) + 1,
    })
  }
  return sortScopeOptions(Array.from(byId.values()))
}

function filterQueueItems(
  items: IngestQueueItemResponse[],
  workspaceFilter: string,
  libraryFilter: string,
  stateFilter: QueueStateFilter,
  search: string,
): IngestQueueItemResponse[] {
  const needle = search.trim().toLowerCase()
  return items.filter((item) => {
    if (workspaceFilter !== ALL_SCOPE_VALUE && item.workspaceId !== workspaceFilter) return false
    if (libraryFilter !== ALL_SCOPE_VALUE && item.libraryId !== libraryFilter) return false
    if (stateFilter === 'running' && item.queueState !== 'leased') return false
    if (stateFilter === 'queued' && item.queueState !== 'queued') return false
    if (stateFilter === 'paused' && item.queueState !== 'paused') return false
    if (!needle) return true
    return [
      item.documentName,
      item.workspaceName,
      item.libraryName,
      item.currentStage,
      item.jobKind,
    ].some((value) => value?.toLowerCase().includes(needle))
  })
}

function queueRefetchInterval(isActive: boolean): number | false {
  if (isActive) return 5000
  return false
}

function QueueInspector({
  selectedItem,
  timelineQuery,
  t,
  isPausePending,
  isResumePending,
  isCancelPending,
  onPause,
  onResume,
  onCancel,
  onOpenDocuments,
}: Readonly<{
  selectedItem: IngestQueueItemResponse | null
  timelineQuery: QueueTimelineQuery
  t: TFunction
  isPausePending: boolean
  isResumePending: boolean
  isCancelPending: boolean
  onPause: (jobId: string) => void
  onResume: (jobId: string) => void
  onCancel: (jobId: string) => void
  onOpenDocuments: (item: IngestQueueItemResponse) => void
}>) {
  if (!selectedItem) {
    return <InspectorPanel empty={t('admin.queueInspectorEmpty')} />
  }
  const stage = stageLabel(selectedItem, t)
  const queuedAt = formatQueueTime(selectedItem.queuedAt)
  const heartbeatAt = formatQueueTime(selectedItem.heartbeatAt)
  const pauseOrResumeAction =
    selectedItem.queueState === 'paused' ? (
      <Button
        size="sm"
        variant="outline"
        className="text-status-ready hover:text-status-ready"
        disabled={!canResume(selectedItem) || isResumePending}
        onClick={() => onResume(selectedItem.jobId)}
        title={isPausing(selectedItem) ? t('admin.queueResumeBlocked') : t('admin.queueResumeJob')}
      >
        <Play className="mr-1.5 h-3.5 w-3.5" />
        {t('admin.queueResumeJob')}
      </Button>
    ) : (
      <Button
        size="sm"
        variant="outline"
        className="text-status-warning hover:text-status-warning"
        disabled={!canPause(selectedItem) || isPausePending}
        onClick={() => onPause(selectedItem.jobId)}
      >
        <Pause className="mr-1.5 h-3.5 w-3.5" />
        {t('admin.queuePauseJob')}
      </Button>
    )

  return (
    <InspectorPanel
      title={selectedItem.documentName}
      titleText={selectedItem.documentName}
      status={
        <StatusBadge tone={stateTone(selectedItem.queueState)}>
          {stateLabel(selectedItem, t)}
        </StatusBadge>
      }
      metrics={[
        {
          label: t('admin.queueInspectorScope'),
          value: selectedItem.libraryName,
          title: selectedItem.libraryName,
        },
        { label: t('admin.queueStage'), value: stage, title: stage },
        { label: t('admin.queueQueuedAt'), value: queuedAt, title: queuedAt },
        { label: t('admin.queueInspectorHeartbeat'), value: heartbeatAt, title: heartbeatAt },
      ]}
      actions={
        <>
          {pauseOrResumeAction}
          <Button size="sm" variant="outline" onClick={() => onOpenDocuments(selectedItem)}>
            <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
            {t('admin.queueOpenDocuments')}
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="text-status-failed hover:text-status-failed"
            disabled={isCancelPending}
            onClick={() => onCancel(selectedItem.jobId)}
          >
            <Square className="mr-1.5 h-3.5 w-3.5" />
            {t('admin.queueCancelJob')}
          </Button>
        </>
      }
    >
      {queueInspectorContent(selectedItem, timelineQuery, t)}
    </InspectorPanel>
  )
}

export function IngestQueueTab({ t, active }: IngestQueueTabProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { workspaces, setActiveWorkspace, setActiveLibrary } = useApp()
  const [search, setSearch] = useState('')
  const [stateFilter, setStateFilter] = useState<QueueStateFilter>('active')
  const [workspaceFilter, setWorkspaceFilter] = useState(ALL_SCOPE_VALUE)
  const [libraryFilter, setLibraryFilter] = useState(ALL_SCOPE_VALUE)
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null)
  const [inspectorOpen, setInspectorOpen] = useState(false)
  const [selectionMode, setSelectionMode] = useState(false)
  const [selectedJobIds, setSelectedJobIds] = useState<Set<string>>(() => new Set())
  const [tableState, setTableState] = useTableState<QueueTableState>({
    tableId: 'admin.ingestQueue',
    defaultValue: DEFAULT_QUEUE_TABLE_STATE,
    parse: parseQueueTableState,
  })
  const [page, setPage] = useState(1)

  const queueQuery = useQuery({
    ...queries.listIngestQueueOptions(),
    queryFn: () => adminApi.listIngestQueue(),
    enabled: active,
    refetchInterval: queueRefetchInterval(active),
  })
  const {
    data: queueData,
    error: queueError,
    isFetching: queueIsFetching,
    isLoading: queueIsLoading,
    refetch: refetchQueue,
  } = queueQuery

  const refreshQueue = useCallback(async () => {
    try {
      await refetchQueue()
    } catch (error) {
      toast.error(errorMessage(error, t('admin.loadQueueFailed')))
    }
  }, [refetchQueue, t])

  const applyQueue = useCallback(
    (queue: IngestQueueResponse) => {
      queryClient.setQueryData(queries.listIngestQueueQueryKey(), queue)
    },
    [queryClient],
  )

  const moveMutation = useMutation({
    mutationFn: ({ jobId, direction }: { jobId: string; direction: IngestQueueMoveDirection }) =>
      adminApi.moveIngestQueueJob(jobId, direction),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t('admin.queueMoveFailed')))
    },
  })

  const cancelMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.cancelIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t('admin.queueCancelFailed')))
    },
  })

  const pauseMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.pauseIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t('admin.queuePauseFailed')))
    },
  })

  const resumeMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.resumeIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t('admin.queueResumeFailed')))
    },
  })

  const bulkQueueMutation = useMutation({
    mutationFn: ({ action, jobIds }: { action: IngestQueueBulkAction; jobIds: string[] }) =>
      adminApi.bulkIngestQueueAction(action, jobIds),
    onSuccess: (
      response: BulkIngestQueueActionResponse,
      variables: { action: IngestQueueBulkAction; jobIds: string[] },
    ) => {
      applyQueue(response.queue)
      const submitted = new Set(variables.jobIds)
      const retained = new Set(Array.from(selectedJobIds).filter((jobId) => !submitted.has(jobId)))
      for (const jobId of response.results
        .filter((result) => result.status !== 'applied')
        .map((result) => result.jobId)) {
        retained.add(jobId)
      }
      setSelectedJobIds(retained)
      setSelectionMode(retained.size > 0)
      const applied = response.results.filter((result) => result.status === 'applied').length
      const notApplied = response.results.length - applied
      if (notApplied > 0) {
        toast.warning(t('admin.queueBulkPartial', { applied, notApplied }))
      } else {
        toast.success(t('admin.queueBulkSuccess', { count: applied }))
      }
    },
    onError: (error) => {
      toast.error(errorMessage(error, t('admin.queueBulkActionFailed')))
    },
  })

  const queueItems = useMemo(() => queueData?.items ?? [], [queueData?.items])
  const workspaceOptions = useMemo(() => buildWorkspaceOptions(queueItems), [queueItems])
  const effectiveWorkspaceFilter = resolveScopeFilter(workspaceFilter, workspaceOptions)

  const libraryOptions = useMemo(
    () => buildLibraryOptions(queueItems, effectiveWorkspaceFilter),
    [effectiveWorkspaceFilter, queueItems],
  )
  const effectiveLibraryFilter = resolveScopeFilter(libraryFilter, libraryOptions)

  const filteredItems = useMemo(
    () =>
      filterQueueItems(
        queueItems,
        effectiveWorkspaceFilter,
        effectiveLibraryFilter,
        stateFilter,
        search,
      ),
    [effectiveLibraryFilter, effectiveWorkspaceFilter, queueItems, search, stateFilter],
  )

  const pageSize = tableState.pageSize
  const totalPages = Math.max(1, Math.ceil(filteredItems.length / pageSize))
  const currentPage = Math.min(page, totalPages)
  const visibleStart = filteredItems.length === 0 ? 0 : (currentPage - 1) * pageSize + 1
  const visibleEnd = Math.min(currentPage * pageSize, filteredItems.length)
  const pagedItems = useMemo(
    () => filteredItems.slice((currentPage - 1) * pageSize, currentPage * pageSize),
    [currentPage, filteredItems, pageSize],
  )
  const selectedItems = useMemo(
    () => filteredItems.filter((item) => selectedJobIds.has(item.jobId)),
    [filteredItems, selectedJobIds],
  )
  const retryableSelectedItems = selectedItems.filter(canRetryRequeue)
  const pausableSelectedItems = selectedItems.filter(canPause)
  const resumableSelectedItems = selectedItems.filter(canResume)
  const cancelableSelectedItems = selectedItems.filter(canCancel)
  const allVisibleSelected =
    pagedItems.length > 0 && pagedItems.every((item) => selectedJobIds.has(item.jobId))

  const selectedItem = useMemo(() => {
    return pagedItems.find((item) => item.jobId === selectedJobId) ?? pagedItems[0] ?? null
  }, [pagedItems, selectedJobId])

  const timelineQuery = useQuery({
    ...queries.listIngestStageEventsOptions({
      path: { attemptId: selectedItem?.attemptId ?? '' },
    }),
    enabled: active && Boolean(selectedItem?.attemptId),
    refetchInterval:
      active && (selectedItem?.queueState === 'leased' || (selectedItem && isPausing(selectedItem)))
        ? 3000
        : false,
  })

  const openDocuments = useCallback(
    async (item: IngestQueueItemResponse) => {
      const workspace = workspaces.find((candidate) => candidate.id === item.workspaceId) ?? {
        id: item.workspaceId,
        name: item.workspaceName,
        createdAt: '',
      }
      setActiveWorkspace(workspace)
      setActiveLibrary({
        id: item.libraryId,
        workspaceId: item.workspaceId,
        name: item.libraryName,
        createdAt: '',
        includeDocumentHintInMcpAnswers: false,
        ingestionReady: true,
        queryReady: true,
        missingBindingPurposes: [],
      })
      const params = new URLSearchParams()
      if (item.documentId) params.set('documentId', item.documentId)
      const queryString = params.toString()
      const documentPath = queryString ? `/documents?${queryString}` : '/documents'
      try {
        await navigate(documentPath)
      } catch (error) {
        toast.error(errorMessage(error, t('admin.queueBulkActionFailed')))
      }
    },
    [navigate, setActiveLibrary, setActiveWorkspace, t, workspaces],
  )

  const movingJobId = moveMutation.variables?.jobId
  const cancelingJobId = cancelMutation.variables
  const pausingJobId = pauseMutation.variables
  const resumingJobId = resumeMutation.variables
  const bulkActionPending = bulkQueueMutation.isPending

  const cancelSelection = () => {
    setSelectionMode(false)
    setSelectedJobIds(new Set())
  }

  const toggleVisibleSelection = () => {
    setSelectedJobIds((current) => {
      const next = new Set(current)
      if (allVisibleSelected) {
        pagedItems.forEach((item) => next.delete(item.jobId))
      } else {
        pagedItems.forEach((item) => next.add(item.jobId))
      }
      return next
    })
  }

  const toggleJobSelection = (jobId: string) => {
    setSelectedJobIds((current) => {
      const next = new Set(current)
      if (next.has(jobId)) {
        next.delete(jobId)
      } else {
        next.add(jobId)
      }
      return next
    })
  }

  const queueRowActions = (item: IngestQueueItemResponse): RowAction[] => [
    {
      key: 'move-up',
      label: t('admin.queueMoveUp'),
      icon: (
        <ArrowUp className={`h-3.5 w-3.5 ${movingJobId === item.jobId ? 'animate-pulse' : ''}`} />
      ),
      disabled: !canMove(item) || moveMutation.isPending,
      onSelect: () => moveMutation.mutate({ jobId: item.jobId, direction: 'up' }),
    },
    {
      key: 'move-down',
      label: t('admin.queueMoveDown'),
      icon: (
        <ArrowDown className={`h-3.5 w-3.5 ${movingJobId === item.jobId ? 'animate-pulse' : ''}`} />
      ),
      disabled: !canMove(item) || moveMutation.isPending,
      onSelect: () => moveMutation.mutate({ jobId: item.jobId, direction: 'down' }),
    },
    item.queueState === 'paused'
      ? {
          key: 'resume',
          label: isPausing(item) ? t('admin.queueResumeBlocked') : t('admin.queueResumeJob'),
          icon: (
            <Play
              className={`h-3.5 w-3.5 ${resumingJobId === item.jobId ? 'animate-pulse' : ''}`}
            />
          ),
          disabled: !canResume(item) || resumeMutation.isPending,
          onSelect: () => resumeMutation.mutate(item.jobId),
        }
      : {
          key: 'pause',
          label: t('admin.queuePauseJob'),
          icon: (
            <Pause
              className={`h-3.5 w-3.5 ${pausingJobId === item.jobId ? 'animate-pulse' : ''}`}
            />
          ),
          disabled: !canPause(item) || pauseMutation.isPending,
          onSelect: () => pauseMutation.mutate(item.jobId),
        },
    {
      key: 'retry-requeue',
      label: t('admin.queueRetryRequeueJob'),
      icon: <RefreshCw className="h-3.5 w-3.5" />,
      disabled: !canRetryRequeue(item),
      onSelect: () => {
        adminApi
          .retryIngestQueueJob(item.jobId)
          .then(applyQueue)
          .catch((error: unknown) => {
            toast.error(errorMessage(error, t('admin.queueRetryRequeueFailed')))
          })
      },
    },
    {
      key: 'documents',
      label: t('admin.queueOpenDocuments'),
      icon: <ExternalLink className="h-3.5 w-3.5" />,
      onSelect: () => {
        openDocuments(item).catch((error: unknown) => {
          toast.error(errorMessage(error, t('admin.queueBulkActionFailed')))
        })
      },
    },
    {
      key: 'cancel',
      label: t('admin.queueCancelJob'),
      icon: (
        <Square className={`h-3.5 w-3.5 ${cancelingJobId === item.jobId ? 'animate-pulse' : ''}`} />
      ),
      disabled: !canCancel(item) || cancelMutation.isPending,
      onSelect: () => cancelMutation.mutate(item.jobId),
      destructive: true,
    },
  ]

  return (
    <div className="flex h-full min-h-0 flex-col">
      <DataWorkspaceView
        className="min-h-0 flex-1"
        inspectorCloseLabel={t('common.close')}
        inspectorLabel={t('admin.queueInspectorTitle')}
        inspectorOpen={inspectorOpen}
        mainClassName="min-h-0"
        onInspectorOpenChange={setInspectorOpen}
        inspector={
          <QueueInspector
            selectedItem={selectedItem}
            timelineQuery={timelineQuery}
            t={t}
            isPausePending={pauseMutation.isPending}
            isResumePending={resumeMutation.isPending}
            isCancelPending={cancelMutation.isPending}
            onPause={(jobId) => pauseMutation.mutate(jobId)}
            onResume={(jobId) => resumeMutation.mutate(jobId)}
            onCancel={(jobId) => cancelMutation.mutate(jobId)}
            onOpenDocuments={openDocuments}
          />
        }
      >
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="grid shrink-0 grid-cols-1 gap-2 border-b bg-surface-sunken/50 px-6 py-3 lg:grid-cols-[minmax(220px,1.3fr)_minmax(180px,0.8fr)_minmax(200px,0.9fr)_minmax(220px,1fr)_auto] lg:items-center">
            <div className="relative min-w-0">
              <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="h-9 pl-9 text-xs"
                placeholder={t('admin.queueSearchPlaceholder')}
                value={search}
                onChange={(event) => {
                  setSearch(event.target.value)
                  setPage(1)
                  setSelectedJobId(null)
                  setSelectedJobIds(new Set())
                }}
              />
            </div>
            <FilterSelect
              ariaLabel={t('admin.queueWorkspaceFilter')}
              className="w-full"
              icon={<Building2 />}
              value={effectiveWorkspaceFilter}
              onValueChange={(value) => {
                setWorkspaceFilter(value)
                setLibraryFilter(ALL_SCOPE_VALUE)
                setPage(1)
                setSelectedJobId(null)
                setSelectedJobIds(new Set())
              }}
            >
              <SelectItem value={ALL_SCOPE_VALUE}>
                {t('admin.queueAllWorkspaces', {
                  count: queueData?.items.length ?? 0,
                })}
              </SelectItem>
              {workspaceOptions.map((option) => (
                <SelectItem key={option.id} value={option.id}>
                  {t('admin.queueFilterOptionWithCount', {
                    count: option.count,
                    name: option.label,
                  })}
                </SelectItem>
              ))}
            </FilterSelect>
            <FilterSelect
              ariaLabel={t('admin.queueLibraryFilter')}
              className="w-full"
              icon={<Boxes />}
              value={effectiveLibraryFilter}
              onValueChange={(value) => {
                setLibraryFilter(value)
                setPage(1)
                setSelectedJobId(null)
                setSelectedJobIds(new Set())
              }}
            >
              <SelectItem value={ALL_SCOPE_VALUE}>
                {t('admin.queueAllLibraries', {
                  count: libraryOptions.reduce((total, option) => total + option.count, 0),
                })}
              </SelectItem>
              {libraryOptions.map((option) => (
                <SelectItem key={option.id} value={option.id}>
                  {t('admin.queueFilterOptionWithCount', {
                    count: option.count,
                    name: option.label,
                  })}
                </SelectItem>
              ))}
            </FilterSelect>
            <FilterSelect
              className="w-full"
              icon={<ListFilter />}
              value={stateFilter}
              onValueChange={(value) => {
                setStateFilter(value as QueueStateFilter)
                setPage(1)
                setSelectedJobId(null)
                setSelectedJobIds(new Set())
              }}
            >
              <SelectItem value="active">
                {t('admin.queueFilterOptionWithCount', {
                  count:
                    (queueData?.summary.running ?? 0) +
                    (queueData?.summary.queued ?? 0) +
                    (queueData?.summary.paused ?? 0),
                  name: t('admin.queueFilterActive'),
                })}
              </SelectItem>
              <SelectItem value="running">
                {t('admin.queueFilterOptionWithCount', {
                  count: queueData?.summary.running ?? 0,
                  name: t('admin.queueFilterRunning'),
                })}
              </SelectItem>
              <SelectItem value="queued">
                {t('admin.queueFilterOptionWithCount', {
                  count: queueData?.summary.queued ?? 0,
                  name: t('admin.queueFilterQueued'),
                })}
              </SelectItem>
              <SelectItem value="paused">
                {t('admin.queueFilterOptionWithCount', {
                  count: queueData?.summary.paused ?? 0,
                  name: t('admin.queueFilterPaused'),
                })}
              </SelectItem>
            </FilterSelect>
            <div className="flex items-center gap-2 lg:justify-self-end">
              <Button
                className="h-9 text-xs"
                onClick={refreshQueue}
                size="sm"
                variant="outline"
                aria-label={t('dashboard.refresh')}
                title={t('dashboard.refresh')}
              >
                <RefreshCw className={`h-3.5 w-3.5 ${queueIsFetching ? 'animate-spin' : ''}`} />
              </Button>
              <Button
                className="h-9 text-xs"
                onClick={selectionMode ? cancelSelection : () => setSelectionMode(true)}
                size="sm"
                variant={selectionMode ? 'default' : 'outline'}
              >
                <CheckSquare className="mr-1.5 h-3.5 w-3.5" />
                {selectionMode ? t('admin.queueCancelSelection') : t('admin.queueSelect')}
              </Button>
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-hidden">
            <DataState
              query={{
                isLoading: queueIsLoading && active,
                error: queueError ? errorMessage(queueError, t('admin.loadQueueFailed')) : null,
                data: queueData,
              }}
              emptyCheck={(queue) => (queue.items ?? []).length === 0}
              emptyRender={<WorkbenchEmptyState title={t('admin.queueEmpty')} />}
            >
              {() => (
                <div className="flex h-full min-h-0 flex-col">
                  <div className="min-h-0 flex-1 overflow-auto">
                    <div className="space-y-3 p-3 xl:hidden">
                      {selectionMode && (
                        <label className="workbench-surface flex items-center gap-2 px-3 py-2 text-xs font-semibold text-muted-foreground">
                          <Checkbox
                            aria-label={t('admin.queueSelectVisible')}
                            checked={allVisibleSelected}
                            onCheckedChange={toggleVisibleSelection}
                          />
                          {t('admin.queueSelectVisible')}
                        </label>
                      )}
                      {pagedItems.map((item) => {
                        const selected = selectedItem?.jobId === item.jobId
                        const progress = progressValue(item)
                        return (
                          <article
                            key={item.jobId}
                            className={`workbench-surface p-4 transition-all ${
                              selected ? 'border-primary/40 bg-primary/5' : ''
                            }`}
                          >
                            <div className="flex items-start gap-3">
                              {selectionMode && (
                                <Checkbox
                                  aria-label={t('admin.queueSelectJob', {
                                    name: item.documentName,
                                  })}
                                  checked={selectedJobIds.has(item.jobId)}
                                  className="mt-1"
                                  onCheckedChange={() => toggleJobSelection(item.jobId)}
                                />
                              )}
                              <button
                                type="button"
                                className="min-w-0 flex-1 text-left"
                                onClick={() => {
                                  setSelectedJobId(item.jobId)
                                  setInspectorOpen(true)
                                }}
                              >
                                <div className="flex items-start justify-between gap-3">
                                  <div className="min-w-0">
                                    <div
                                      className="truncate text-sm font-bold"
                                      title={item.documentName}
                                    >
                                      {item.documentName}
                                    </div>
                                    <div className="mt-1 text-xs text-muted-foreground">
                                      {item.libraryName} · {item.workspaceName}
                                    </div>
                                  </div>
                                  <StatusBadge
                                    tone={stateTone(item.queueState)}
                                    className="shrink-0"
                                  >
                                    {stateLabel(item, t)}
                                  </StatusBadge>
                                </div>
                                <div className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
                                  <span className="text-muted-foreground">
                                    {t('admin.queueOrder')}
                                  </span>
                                  <span className="font-mono font-semibold">
                                    {item.queueState === 'leased'
                                      ? t('admin.queueNow')
                                      : `#${item.queuePosition ?? '—'}`}
                                  </span>
                                  <span className="text-muted-foreground">
                                    {t('admin.queueStage')}
                                  </span>
                                  <span className="truncate font-semibold">
                                    {stageLabel(item, t)}
                                  </span>
                                  <span className="text-muted-foreground">
                                    {t('admin.queueQueuedAt')}
                                  </span>
                                  <span className="truncate">{formatQueueTime(item.queuedAt)}</span>
                                </div>
                              </button>
                            </div>
                            <div className="mt-4">
                              <div className="mb-1 flex items-center justify-between text-xs">
                                <span className="text-muted-foreground">
                                  {queueProgressLabel(item, t)}
                                </span>
                                <span className="font-mono font-semibold">{progress}%</span>
                              </div>
                              <div className="h-2 rounded-full bg-muted">
                                <div
                                  className="h-full rounded-full bg-primary transition-all"
                                  style={{ width: `${progress}%` }}
                                />
                              </div>
                            </div>
                            <QueueFailureNotice item={item} t={t} />
                            <div className="mt-4 flex justify-end">
                              <RowActionsMenu
                                actions={queueRowActions(item)}
                                className="w-full sm:w-8"
                                label={t('documents.actions')}
                              />
                            </div>
                          </article>
                        )
                      })}
                    </div>
                    <table className="hidden w-full min-w-[1000px] table-fixed text-sm xl:table">
                      <colgroup>
                        {selectionMode && <col className="w-[4%]" />}
                        <col className="w-[7%]" />
                        <col className={selectionMode ? 'w-[22%]' : 'w-[25%]'} />
                        <col className="w-[17%]" />
                        <col className="w-[12%]" />
                        <col className="w-[14%]" />
                        <col className="w-[13%]" />
                        <col className="w-[12%]" />
                      </colgroup>
                      <thead className="sticky top-0 z-10 bg-card">
                        <tr className="border-b text-left">
                          {selectionMode && (
                            <th className="px-4 py-3">
                              <Checkbox
                                aria-label={t('admin.queueSelectVisible')}
                                checked={allVisibleSelected}
                                onCheckedChange={toggleVisibleSelection}
                              />
                            </th>
                          )}
                          <th className="px-4 py-3 section-label">{t('admin.queueOrder')}</th>
                          <th className="px-4 py-3 section-label">{t('documents.name')}</th>
                          <th className="px-4 py-3 section-label">{t('admin.scope')}</th>
                          <th className="px-4 py-3 section-label">{t('admin.status')}</th>
                          <th className="px-4 py-3 section-label">{t('admin.queueStage')}</th>
                          <th className="px-4 py-3 section-label">{t('admin.queueQueuedAt')}</th>
                          <th className="px-4 py-3 section-label text-right">
                            {t('admin.queueActions')}
                          </th>
                        </tr>
                      </thead>
                      <tbody>
                        {pagedItems.map((item) => {
                          const selected = selectedItem?.jobId === item.jobId
                          return (
                            <Fragment key={item.jobId}>
                              <tr
                                className={`cursor-pointer border-b border-border/50 transition-colors ${queueRowClassName(selected, Boolean(item.failureMessage))}`}
                                onClick={() => {
                                  setSelectedJobId(item.jobId)
                                  setInspectorOpen(true)
                                }}
                              >
                                {selectionMode && (
                                  <td className="px-4 py-3">
                                    <Checkbox
                                      aria-label={t('admin.queueSelectJob', {
                                        name: item.documentName,
                                      })}
                                      checked={selectedJobIds.has(item.jobId)}
                                      onClick={(event) => event.stopPropagation()}
                                      onCheckedChange={() => toggleJobSelection(item.jobId)}
                                    />
                                  </td>
                                )}
                                <td className="px-4 py-3 font-mono text-xs text-muted-foreground">
                                  {item.queueState === 'leased'
                                    ? t('admin.queueNow')
                                    : `#${item.queuePosition ?? '—'}`}
                                </td>
                                <td className="px-4 py-3">
                                  <div
                                    className="max-w-md truncate font-semibold"
                                    title={item.documentName}
                                  >
                                    {item.documentName}
                                  </div>
                                  <div className="mt-1 text-2xs text-muted-foreground">
                                    {item.jobKind}
                                  </div>
                                </td>
                                <td className="px-4 py-3 text-xs">
                                  <div className="font-semibold">{item.libraryName}</div>
                                  <div className="mt-1 text-muted-foreground">
                                    {item.workspaceName}
                                  </div>
                                </td>
                                <td className="px-4 py-3">
                                  <StatusBadge tone={stateTone(item.queueState)}>
                                    {stateLabel(item, t)}
                                  </StatusBadge>
                                </td>
                                <td className="px-4 py-3 text-xs">
                                  <div className="font-medium">{stageLabel(item, t)}</div>
                                  <div className="mt-1 text-muted-foreground">
                                    {queueProgressLabel(item, t)}
                                  </div>
                                </td>
                                <td className="px-4 py-3 text-xs text-muted-foreground">
                                  <div>{formatQueueTime(item.queuedAt)}</div>
                                  {item.startedAt && (
                                    <div className="mt-1">
                                      {t('admin.queueStartedAt', {
                                        value: formatQueueTime(item.startedAt),
                                      })}
                                    </div>
                                  )}
                                </td>
                                <td className="px-4 py-3">
                                  <RowActionsMenu
                                    actions={queueRowActions(item)}
                                    label={t('documents.actions')}
                                  />
                                </td>
                              </tr>
                              {item.failureMessage ? (
                                <tr
                                  className={
                                    selected
                                      ? 'bg-primary/5'
                                      : 'border-l-2 border-l-destructive/60 bg-destructive/[0.03]'
                                  }
                                >
                                  <td
                                    className="border-b border-border/50 px-4 pb-3 pt-0"
                                    colSpan={selectionMode ? 8 : 7}
                                  >
                                    <QueueFailureNotice compact item={item} t={t} />
                                  </td>
                                </tr>
                              ) : null}
                            </Fragment>
                          )
                        })}
                      </tbody>
                    </table>
                    {filteredItems.length === 0 && (
                      <WorkbenchEmptyState title={t('admin.queueNoMatches')} />
                    )}
                  </div>
                  {filteredItems.length > 0 && (
                    <>
                      <QueueBulkBar
                        bulkActionPending={bulkActionPending}
                        cancelCount={cancelableSelectedItems.length}
                        onCancel={() =>
                          bulkQueueMutation.mutate({
                            action: 'cancel',
                            jobIds: cancelableSelectedItems.map((item) => item.jobId),
                          })
                        }
                        onClear={() => setSelectedJobIds(new Set())}
                        onPause={() =>
                          bulkQueueMutation.mutate({
                            action: 'pause',
                            jobIds: pausableSelectedItems.map((item) => item.jobId),
                          })
                        }
                        onRetryRequeue={() =>
                          bulkQueueMutation.mutate({
                            action: 'retry_requeue',
                            jobIds: retryableSelectedItems.map((item) => item.jobId),
                          })
                        }
                        onResume={() =>
                          bulkQueueMutation.mutate({
                            action: 'resume',
                            jobIds: resumableSelectedItems.map((item) => item.jobId),
                          })
                        }
                        pauseCount={pausableSelectedItems.length}
                        retryRequeueCount={retryableSelectedItems.length}
                        resumeCount={resumableSelectedItems.length}
                        selectedCount={selectedItems.length}
                        t={t}
                      />
                      <QueuePaginationFooter
                        currentPage={currentPage}
                        pageSize={pageSize}
                        t={t}
                        totalItems={filteredItems.length}
                        totalPages={totalPages}
                        visibleEnd={visibleEnd}
                        visibleStart={visibleStart}
                        onPageSizeChange={(nextPageSize) => {
                          setTableState({ pageSize: nextPageSize })
                          setPage(1)
                          setSelectedJobId(null)
                          setSelectedJobIds(new Set())
                        }}
                        onGoToPage={(target) => {
                          setPage(target)
                          setSelectedJobId(null)
                          setSelectedJobIds(new Set())
                        }}
                      />
                    </>
                  )}
                </div>
              )}
            </DataState>
          </div>
        </div>
      </DataWorkspaceView>
    </div>
  )
}

function QueueFailureNotice({
  compact = false,
  item,
  t,
}: Readonly<{
  compact?: boolean
  item: IngestQueueItemResponse
  t: TFunction
}>) {
  const [expanded, setExpanded] = useState(false)

  if (!item.failureMessage) return null
  const paused = item.queueState === 'paused'

  // Paused jobs are not failures — keep a calm, minimal notice.
  if (paused) {
    return (
      <div className="mt-2 rounded-md border-l-2 border-status-warning/60 bg-status-warning/5 px-3 py-2 text-xs">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <span className="font-bold text-status-warning">{t('admin.queueStatePaused')}</span>
          <span className="min-w-0 truncate text-2xs font-normal text-muted-foreground">
            {item.documentName}
          </span>
        </div>
        <div className="mt-1 whitespace-pre-wrap break-words text-foreground">
          {item.failureMessage}
        </div>
      </div>
    )
  }

  // Reuse the shared document-failure taxonomy so a failed job explains BOTH
  // what happened (summary) and how to fix it (action) instead of a raw
  // backend string; the technical code + message stay behind the toggle.
  const notice = buildDocumentFailureNotice(
    {
      failureCode: item.failureCode ?? null,
      failureMessage: item.failureMessage,
      stage: item.currentStage ?? null,
    },
    t,
  )
  const summary = notice?.summary ?? item.failureMessage
  const diagnosticCode = notice?.diagnosticCode ?? item.failureCode ?? undefined
  const diagnosticMessage = notice?.diagnosticMessage
  const hasDetails = Boolean(diagnosticCode || diagnosticMessage)
  const showDetails = !compact || expanded

  return (
    <div className="mt-2 rounded-md border-l-2 border-destructive/60 bg-destructive/5 px-3 py-2 text-xs">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="font-bold text-destructive">
          {notice?.title ?? t('admin.queueFailure')}
        </span>
        <span className="min-w-0 truncate text-2xs font-normal text-muted-foreground">
          {item.documentName}
        </span>
      </div>
      <div className="mt-1 whitespace-pre-wrap break-words text-foreground">{summary}</div>
      {notice?.action ? (
        <div className="mt-1.5 flex items-start gap-1.5 text-2xs text-muted-foreground">
          <Wrench className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{notice.action}</span>
        </div>
      ) : null}
      {hasDetails && showDetails ? (
        <div className="mt-1.5 space-y-0.5 border-t border-destructive/15 pt-1.5 text-2xs text-muted-foreground">
          {diagnosticCode ? (
            <div className="truncate">
              {t('admin.queueFailureCode')}: <code className="font-mono">{diagnosticCode}</code>
            </div>
          ) : null}
          {diagnosticMessage ? (
            <div className="whitespace-pre-wrap break-words font-mono">{diagnosticMessage}</div>
          ) : null}
        </div>
      ) : null}
      {hasDetails && compact ? (
        <button
          type="button"
          className="mt-1 inline-flex text-2xs font-semibold text-muted-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onClick={(event) => {
            event.stopPropagation()
            setExpanded((value) => !value)
          }}
        >
          {expanded ? t('admin.queueFailureCollapse') : t('admin.queueFailureExpand')}
        </button>
      ) : null}
    </div>
  )
}

type QueuePaginationFooterProps = Readonly<{
  currentPage: number
  onGoToPage: (page: number) => void
  onPageSizeChange: (pageSize: QueuePageSize) => void
  pageSize: QueuePageSize
  t: TFunction
  totalItems: number
  totalPages: number
  visibleEnd: number
  visibleStart: number
}>

type QueueBulkBarProps = Readonly<{
  bulkActionPending: boolean
  cancelCount: number
  onCancel: () => void
  onClear: () => void
  onPause: () => void
  onRetryRequeue: () => void
  onResume: () => void
  pauseCount: number
  retryRequeueCount: number
  resumeCount: number
  selectedCount: number
  t: TFunction
}>

function QueueBulkBar({
  bulkActionPending,
  cancelCount,
  onCancel,
  onClear,
  onPause,
  onRetryRequeue,
  onResume,
  pauseCount,
  retryRequeueCount,
  resumeCount,
  selectedCount,
  t,
}: QueueBulkBarProps) {
  if (selectedCount <= 0) return null
  return (
    <div className="shrink-0 border-t bg-primary/5 px-4 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="mr-auto text-xs font-semibold text-primary tabular-nums">
          {t('admin.queueSelected', { count: selectedCount })}
        </span>
        <Button
          className="h-8 text-xs"
          disabled={retryRequeueCount <= 0 || bulkActionPending}
          onClick={onRetryRequeue}
          size="sm"
          variant="outline"
        >
          <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
          {t('admin.queueRetryRequeueSelected', { count: retryRequeueCount })}
        </Button>
        <Button
          className="h-8 text-xs"
          disabled={pauseCount <= 0 || bulkActionPending}
          onClick={onPause}
          size="sm"
          variant="outline"
        >
          <Pause className="mr-1.5 h-3.5 w-3.5" />
          {t('admin.queuePauseSelected', { count: pauseCount })}
        </Button>
        <Button
          className="h-8 text-xs"
          disabled={resumeCount <= 0 || bulkActionPending}
          onClick={onResume}
          size="sm"
          variant="outline"
        >
          <Play className="mr-1.5 h-3.5 w-3.5" />
          {t('admin.queueResumeSelected', { count: resumeCount })}
        </Button>
        <Button
          className="h-8 text-xs text-status-failed hover:text-status-failed"
          disabled={cancelCount <= 0 || bulkActionPending}
          onClick={onCancel}
          size="sm"
          variant="outline"
        >
          <Square className="mr-1.5 h-3.5 w-3.5" />
          {t('admin.queueCancelSelected', { count: cancelCount })}
        </Button>
        <Button
          className="h-8 text-xs"
          disabled={bulkActionPending}
          onClick={onClear}
          size="sm"
          variant="ghost"
        >
          {t('admin.queueClearSelection')}
        </Button>
      </div>
    </div>
  )
}

function QueuePaginationFooter({
  currentPage,
  onGoToPage,
  onPageSizeChange,
  pageSize,
  t,
  totalItems,
  totalPages,
  visibleEnd,
  visibleStart,
}: QueuePaginationFooterProps) {
  return (
    <TablePaginationFooter
      canGoNext={currentPage < totalPages}
      canGoPrevious={currentPage > 1}
      currentPageNumber={currentPage}
      goToNextPage={() => onGoToPage(Math.min(totalPages, currentPage + 1))}
      goToPage={onGoToPage}
      goToPreviousPage={() => onGoToPage(Math.max(1, currentPage - 1))}
      nextLabel={t('documents.next')}
      onPageSizeChange={onPageSizeChange}
      pageSize={pageSize}
      pageSizeLabel={t('documents.pageSize')}
      pageSizeOptions={TABLE_PAGE_SIZE_OPTIONS}
      previousLabel={t('documents.previous')}
      summary={t('documents.paginationSummary', {
        from: visibleStart,
        to: visibleEnd,
        total: totalItems,
      })}
      totalPages={totalPages}
    />
  )
}
