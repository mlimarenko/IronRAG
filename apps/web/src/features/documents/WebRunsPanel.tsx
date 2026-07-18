import { Suspense, useDeferredValue, useMemo, useState } from 'react'
import type { TFunction } from 'i18next'
import {
  Copy,
  ExternalLink,
  Globe,
  ListFilter,
  Loader2,
  RotateCw,
  Search,
  SquareX,
} from 'lucide-react'
import { toast } from 'sonner'
import { useSuspenseQuery } from '@tanstack/react-query'

import { queries, type WebIngestRunListItem, type WebIngestRunPageItem } from '@/shared/api'
import { Button } from '@/shared/components/ui/button'
import { FilterSelect } from '@/shared/components/FilterSelect'
import { Input } from '@/shared/components/ui/input'
import { ScrollArea } from '@/shared/components/ui/scroll-area'
import { SelectItem } from '@/shared/components/ui/select'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import { StatusBadge, type StatusTone } from '@/shared/components/StatusBadge'
import { cn } from '@/shared/lib/utils'

const TERMINAL_RUN_STATES = new Set(['completed', 'completed_partial', 'failed', 'canceled'])
const PAGE_WINDOW_SIZE = 200
const RUN_COUNT_ORDER = [
  'processed',
  'processing',
  'queued',
  'failed',
  'excluded',
  'duplicates',
  'blocked',
  'canceled',
  'eligible',
  'discovered',
] as const
const PAGE_STATE_ORDER = [
  'processed',
  'materialized',
  'failed',
  'excluded',
  'duplicates',
  'blocked',
  'queued',
  'processing',
  'eligible',
  'discovered',
  'canceled',
] as const

function runStatusTone(state: string): StatusTone {
  switch (state) {
    case 'completed':
      return 'ready'
    case 'completed_partial':
      return 'warning'
    case 'failed':
      return 'failed'
    case 'canceled':
      return 'stalled'
    default:
      return 'processing'
  }
}

function pageStateDotClass(state: string | undefined): string {
  switch (state) {
    case 'processed':
      return 'bg-status-ready'
    case 'failed':
      return 'bg-status-failed'
    case 'excluded':
      return 'bg-status-warning'
    case 'duplicates':
      return 'bg-status-processing'
    case 'blocked':
      return 'bg-primary'
    case 'queued':
      return 'bg-status-queued'
    case 'processing':
    case 'materialized':
      return 'bg-status-warning'
    case 'canceled':
      return 'bg-status-stalled'
    default:
      return 'bg-muted-foreground'
  }
}

function humanizeRunMode(mode: string, t: TFunction): string {
  if (mode === 'single_page') return t('documents.singlePage')
  if (mode === 'recursive_crawl') return t('documents.recursiveCrawl')
  return mode
}

function humanizeRunState(state: string, t: TFunction): string {
  const key = `dashboard.runStateLabels.${state}`
  const translated = t(key)
  return translated === key ? state.replace(/_/g, ' ') : translated
}

function humanizePageState(state: string, t: TFunction): string {
  const translated = t(`documents.pageStateLabels.${state}`)
  return translated === `documents.pageStateLabels.${state}` ? state.replace(/_/g, ' ') : translated
}

function filterPatternCount(filter: WebIngestRunListItem['crawlFilter'] | undefined): number {
  return (filter?.allowPatterns?.length ?? 0) + (filter?.blockPatterns?.length ?? 0)
}

function pagePrimaryUrl(page: WebIngestRunPageItem): string {
  return page.finalUrl ?? page.canonicalUrl ?? page.normalizedUrl ?? page.discoveredUrl ?? ''
}

function sortStates(states: string[]): string[] {
  return [...states].sort((a, b) => {
    const aIndex = PAGE_STATE_ORDER.indexOf(a as (typeof PAGE_STATE_ORDER)[number])
    const bIndex = PAGE_STATE_ORDER.indexOf(b as (typeof PAGE_STATE_ORDER)[number])
    if (aIndex === -1 && bIndex === -1) return a.localeCompare(b)
    if (aIndex === -1) return 1
    if (bIndex === -1) return -1
    return aIndex - bIndex
  })
}

type WebRunsPanelProps = {
  t: TFunction
  webRuns: WebIngestRunListItem[]
  onReuseRun: (run: WebIngestRunListItem) => void
  onCancelRun: (runId: string) => Promise<void>
}

type ExpandedRunPagesProps = {
  t: TFunction
  run: WebIngestRunListItem
  onOpenUrl: (url: string) => void
  onCopyUrl: (url: string) => Promise<void>
}

function WebRunPagesFallback({ t }: Readonly<{ t: TFunction }>) {
  return (
    <div role="status" aria-live="polite">
      <div className="border-b px-4 py-3">
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
          {t('documents.loadingPages')}
        </div>
      </div>
      <div className="space-y-2 px-4 py-3">
        {['first', 'second', 'third'].map((placeholder) => (
          <div key={placeholder} className="h-14 rounded-xl border bg-background/70" />
        ))}
      </div>
    </div>
  )
}

function ExpandedRunPages({ t, run, onOpenUrl, onCopyUrl }: Readonly<ExpandedRunPagesProps>) {
  const [pageStateFilter, setPageStateFilter] = useState<string>('all')
  const [pageSearch, setPageSearch] = useState('')
  const [pageWindowIndex, setPageWindowIndex] = useState(0)

  const runPagesQuery = useSuspenseQuery({
    ...queries.listContentWebIngestRunPagesOptions({
      path: { runId: run.runId },
    }),
  })

  const runPages: WebIngestRunPageItem[] = useMemo(
    () => runPagesQuery.data ?? [],
    [runPagesQuery.data],
  )
  const runPagesRefreshing = runPagesQuery.isFetching
  const deferredPageSearch = useDeferredValue(pageSearch.trim().toLowerCase())

  const runSummaryItems = useMemo(() => {
    if (!run.counts) return []
    return RUN_COUNT_ORDER.map((key) => ({
      key,
      value: run.counts?.[key],
    })).filter((item) => (item.value ?? 0) > 0)
  }, [run.counts])

  const pageStateCounts = useMemo(() => {
    const counts = new Map<string, number>()
    for (const page of runPages) {
      const state = page.candidateState ?? 'unknown'
      counts.set(state, (counts.get(state) ?? 0) + 1)
    }
    return counts
  }, [runPages])

  const availablePageStates = useMemo(
    () => sortStates([...pageStateCounts.keys()].filter((state) => state !== 'unknown')),
    [pageStateCounts],
  )

  const filteredRunPages = useMemo(() => {
    return runPages.filter((page) => {
      if (pageStateFilter !== 'all' && (page.candidateState ?? 'unknown') !== pageStateFilter) {
        return false
      }
      if (!deferredPageSearch) {
        return true
      }
      const haystack = [
        pagePrimaryUrl(page),
        page.discoveredUrl,
        page.classificationReason,
        page.classificationDetail,
        page.contentType,
      ]
        .filter(Boolean)
        .join(' ')
        .toLowerCase()
      return haystack.includes(deferredPageSearch)
    })
  }, [deferredPageSearch, pageStateFilter, runPages])

  const totalPageWindows = Math.max(1, Math.ceil(filteredRunPages.length / PAGE_WINDOW_SIZE))
  const visiblePageWindowIndex = Math.min(pageWindowIndex, totalPageWindows - 1)

  const visiblePages = useMemo(() => {
    const start = visiblePageWindowIndex * PAGE_WINDOW_SIZE
    return filteredRunPages.slice(start, start + PAGE_WINDOW_SIZE)
  }, [filteredRunPages, visiblePageWindowIndex])

  const visibleRangeStart =
    filteredRunPages.length === 0 ? 0 : visiblePageWindowIndex * PAGE_WINDOW_SIZE + 1
  const visibleRangeEnd =
    filteredRunPages.length === 0
      ? 0
      : Math.min(filteredRunPages.length, (visiblePageWindowIndex + 1) * PAGE_WINDOW_SIZE)

  return (
    <>
      <div className="border-b px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          {runSummaryItems.map((item) => (
            <span
              key={item.key}
              className="rounded-full border bg-background px-2.5 py-1 text-2xs text-muted-foreground"
            >
              {humanizePageState(item.key, t)}:{' '}
              <span className="font-semibold text-foreground">
                {(item.value ?? 0).toLocaleString()}
              </span>
            </span>
          ))}
        </div>

        <div className="mt-3 flex flex-col gap-3 xl:flex-row xl:items-center">
          <div className="relative w-full xl:max-w-sm">
            <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={pageSearch}
              onChange={(event) => {
                setPageSearch(event.target.value)
                setPageWindowIndex(0)
              }}
              className="h-8 pl-9 text-xs"
              placeholder={t('documents.pageSearchPlaceholder')}
            />
          </div>
          <FilterSelect
            value={pageStateFilter}
            onValueChange={(state) => {
              setPageStateFilter(state)
              setPageWindowIndex(0)
            }}
            icon={<ListFilter />}
            className="w-[160px]"
          >
            <SelectItem value="all">{t('documents.all')}</SelectItem>
            {availablePageStates.map((state) => (
              <SelectItem key={state} value={state}>
                {humanizePageState(state, t)}
              </SelectItem>
            ))}
          </FilterSelect>
          <div className="flex items-center gap-2 xl:ml-auto">
            <span className="text-2xs text-muted-foreground">
              {t('documents.pageWindowSummary', {
                from: visibleRangeStart,
                to: visibleRangeEnd,
                total: filteredRunPages.length,
              })}
            </span>
            <Button
              variant="outline"
              size="sm"
              className="h-8 text-xs"
              disabled={runPagesRefreshing}
              onClick={async () => {
                try {
                  await runPagesQuery.refetch()
                } catch {
                  toast.error(t('documents.failedToLoad'))
                }
              }}
            >
              {runPagesRefreshing ? (
                <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
              ) : (
                <RotateCw className="mr-1.5 h-3.5 w-3.5" />
              )}
              {t('documents.refreshRunPages')}
            </Button>
          </div>
        </div>
      </div>

      {filteredRunPages.length === 0 ? (
        <WorkbenchEmptyState
          className="py-8"
          title={t('documents.noMatchingPages')}
          description={
            runPages.length === 0
              ? t('documents.noMatchingPagesDesc')
              : t('documents.noMatchingPagesFilteredDesc')
          }
        />
      ) : (
        <>
          <div className="space-y-2 px-4 py-3">
            {visiblePages.map((page) => {
              const url = pagePrimaryUrl(page)
              return (
                <div
                  key={page.candidateId ?? `${page.runId}-${url}`}
                  className="flex items-start gap-3 rounded-xl border bg-background/80 px-3 py-2.5"
                >
                  <span
                    className={cn(
                      'mt-1.5 h-2 w-2 shrink-0 rounded-full',
                      pageStateDotClass(page.candidateState),
                    )}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-xs font-medium" title={url}>
                      {url || '?'}
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-muted-foreground">
                      <span>{humanizePageState(page.candidateState ?? 'unknown', t)}</span>
                      {page.depth != null && (
                        <span>
                          {t('documents.maxDepth')}: {page.depth}
                        </span>
                      )}
                      {page.httpStatus != null && <span>HTTP {page.httpStatus}</span>}
                      {page.contentType && <span>{page.contentType}</span>}
                      {page.classificationReason && (
                        <span title={page.classificationReason}>{page.classificationReason}</span>
                      )}
                      {page.classificationDetail && (
                        <span className="max-w-full truncate" title={page.classificationDetail}>
                          {page.classificationDetail}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      title={t('documents.openPage')}
                      aria-label={t('documents.openPage')}
                      onClick={() => onOpenUrl(url)}
                    >
                      <ExternalLink className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      title={t('documents.copyUrl')}
                      aria-label={t('documents.copyUrl')}
                      onClick={async () => {
                        await onCopyUrl(url)
                      }}
                    >
                      <Copy className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              )
            })}
          </div>

          {filteredRunPages.length > PAGE_WINDOW_SIZE && (
            <div className="flex items-center justify-between border-t px-4 py-3">
              <span className="text-2xs text-muted-foreground">
                {t('documents.pageLabel', {
                  page: visiblePageWindowIndex + 1,
                  total: totalPageWindows,
                })}
              </span>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 text-xs"
                  disabled={visiblePageWindowIndex === 0}
                  onClick={() => setPageWindowIndex(Math.max(0, visiblePageWindowIndex - 1))}
                >
                  {t('documents.previous')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 text-xs"
                  disabled={visiblePageWindowIndex >= totalPageWindows - 1}
                  onClick={() =>
                    setPageWindowIndex(Math.min(totalPageWindows - 1, visiblePageWindowIndex + 1))
                  }
                >
                  {t('documents.next')}
                </Button>
              </div>
            </div>
          )}
        </>
      )}
    </>
  )
}

export function WebRunsPanel({ t, webRuns, onReuseRun, onCancelRun }: Readonly<WebRunsPanelProps>) {
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null)
  const [cancelingRunId, setCancelingRunId] = useState<string | null>(null)

  const activeRuns = webRuns.filter(
    (run) => !TERMINAL_RUN_STATES.has(run.runState?.toLowerCase() ?? ''),
  )

  const handleToggleRun = (runId: string) => {
    if (expandedRunId === runId) {
      setExpandedRunId(null)
      return
    }
    setExpandedRunId(runId)
  }

  const handleOpenUrl = (url: string) => {
    if (!url) return
    window.open(url, '_blank', 'noopener,noreferrer')
  }

  const handleCopyUrl = async (url: string) => {
    if (!url) return
    try {
      await navigator.clipboard.writeText(url)
      toast.success(t('documents.urlCopied'))
    } catch {
      toast.error(t('documents.urlCopyFailed'))
    }
  }

  const handleCancelRun = async (runId: string) => {
    setCancelingRunId(runId)
    try {
      await onCancelRun(runId)
    } catch {
      toast.error(t('documents.webIngestCancelFailed'))
    } finally {
      setCancelingRunId(null)
    }
  }

  if (webRuns.length === 0) {
    return (
      <WorkbenchEmptyState
        icon={<Globe className="h-7 w-7 text-muted-foreground" />}
        title={t('documents.webIngestRuns')}
        description={t('documents.noWebRunsDesc')}
      />
    )
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {activeRuns.length > 0 && (
        <div className="mx-4 mt-4 flex flex-wrap items-center gap-2">
          <div className="workbench-surface flex items-center gap-2 px-3 py-2 text-xs">
            <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
            <span className="font-semibold">
              {t('documents.webRunActiveSummary', { count: activeRuns.length })}
            </span>
          </div>
        </div>
      )}

      {/* Card + single ScrollArea is the whole scroll contract. The
          single ScrollArea wraps the whole list — nesting a second
          ScrollArea around the page-list traps the wheel event on the
          inner one and makes the outer run list look "frozen" once a
          single run is expanded. Refresh button and run-count header
          live in DocumentsPageHeader and are not duplicated here. */}
      <div className="workbench-surface m-4 flex min-h-0 flex-1 flex-col overflow-hidden">
        <ScrollArea className="min-h-0 flex-1">
          <div className="divide-y">
            {webRuns.map((run) => {
              const isExpanded = expandedRunId === run.runId
              const isCancelable = !TERMINAL_RUN_STATES.has(run.runState?.toLowerCase() ?? '')
              return (
                <div key={run.runId}>
                  <div
                    className={cn('flex items-start gap-3 px-4 py-3', isExpanded && 'bg-accent/20')}
                  >
                    <button
                      type="button"
                      className="min-w-0 flex-1 text-left"
                      onClick={() => handleToggleRun(run.runId)}
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <StatusBadge tone={runStatusTone(run.runState)}>
                          {humanizeRunState(run.runState, t)}
                        </StatusBadge>
                        <span className="truncate text-sm font-semibold" title={run.seedUrl}>
                          {run.seedUrl}
                        </span>
                      </div>
                      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-muted-foreground">
                        <span>{humanizeRunMode(run.mode, t)}</span>
                        {run.mode === 'recursive_crawl' && (
                          <span>
                            {t('documents.maxDepth')}: {run.maxDepth} · {t('documents.maxPages')}:{' '}
                            {run.maxPages}
                          </span>
                        )}
                        <span>
                          {t('documents.crawlFilterTitle')}:{' '}
                          {filterPatternCount(run.crawlFilter).toLocaleString()} ·{' '}
                          {t('documents.materializationFilterTitle')}:{' '}
                          {filterPatternCount(run.materializationFilter).toLocaleString()}
                        </span>
                        <span>
                          {(run.counts?.processed ?? 0).toLocaleString()} /{' '}
                          {(run.counts?.discovered ?? 0).toLocaleString()} {t('documents.pages')}
                        </span>
                        {(run.counts?.failed ?? 0) > 0 && (
                          <span>
                            {humanizePageState('failed', t)}:{' '}
                            {(run.counts?.failed ?? 0).toLocaleString()}
                          </span>
                        )}
                        {(run.counts?.excluded ?? 0) > 0 && (
                          <span>
                            {humanizePageState('excluded', t)}:{' '}
                            {(run.counts?.excluded ?? 0).toLocaleString()}
                          </span>
                        )}
                      </div>
                    </button>

                    <div className="flex shrink-0 items-center gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t('documents.openRunUrl')}
                        aria-label={t('documents.openRunUrl')}
                        onClick={() => handleOpenUrl(run.seedUrl)}
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t('documents.copyUrl')}
                        aria-label={t('documents.copyUrl')}
                        onClick={async () => {
                          await handleCopyUrl(run.seedUrl)
                        }}
                      >
                        <Copy className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t('documents.reuseRunSettings')}
                        aria-label={t('documents.reuseRunSettings')}
                        onClick={() => onReuseRun(run)}
                      >
                        <RotateCw className="h-3.5 w-3.5" />
                      </Button>
                      {isCancelable && (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-8 w-8 text-destructive hover:text-destructive"
                          disabled={cancelingRunId === run.runId}
                          title={t('documents.cancelRun')}
                          aria-label={t('documents.cancelRun')}
                          onClick={async () => {
                            await handleCancelRun(run.runId)
                          }}
                        >
                          {cancelingRunId === run.runId ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <SquareX className="h-3.5 w-3.5" />
                          )}
                        </Button>
                      )}
                    </div>
                  </div>

                  {isExpanded && (
                    <div className="border-t bg-surface-sunken/35">
                      <Suspense fallback={<WebRunPagesFallback t={t} />}>
                        <ExpandedRunPages
                          t={t}
                          run={run}
                          onCopyUrl={handleCopyUrl}
                          onOpenUrl={handleOpenUrl}
                        />
                      </Suspense>
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        </ScrollArea>
      </div>
    </div>
  )
}
