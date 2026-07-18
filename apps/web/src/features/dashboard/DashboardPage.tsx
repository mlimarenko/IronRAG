import { Suspense, useCallback, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import { Activity, FileText, MessageSquare, RefreshCw, XCircle } from 'lucide-react'

import { Button } from '@/shared/components/ui/button'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'
import { useApp } from '@/shared/contexts/app-context'
import { useLibraryMetrics } from '@/features/dashboard/hooks/useLibraryMetrics'

import { SummaryCards, type SummaryCard } from './components/SummaryCards'
import { LibraryHealthPanel, type HealthRow } from './components/LibraryHealthPanel'
import { RecentDocumentsList } from './components/RecentDocumentsList'
import { AttentionPanel } from './components/AttentionPanel'
import { LatestIngestPanel } from './components/LatestIngestPanel'
import { DashboardSkeleton } from './components/DashboardSkeleton'
import { DashboardEmptyState } from './components/DashboardEmptyState'
import type { RecentWebRun } from './model/types'
import { buildDocumentsPath } from './model/types'

function pickLatestRun(runs: RecentWebRun[]): RecentWebRun | undefined {
  let latest: RecentWebRun | undefined
  let latestTs = -Infinity
  for (const run of runs) {
    const ts = run.lastActivityAt ? new Date(run.lastActivityAt).getTime() : 0
    if (ts > latestTs) {
      latestTs = ts
      latest = run
    }
  }
  return latest
}

function DashboardContent({
  activeLibraryId,
  queryReady,
}: Readonly<{
  activeLibraryId: string
  queryReady: boolean
}>) {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()

  // Canonical live-metrics path: a shared hook polls the dashboard
  // endpoint every 2.5 s while the tab is visible, pauses on hide,
  // and fires an immediate refresh when the tab resumes. That stops
  // the "number frozen since yesterday" class of bugs — operators
  // see live-changing counts without any refresh clicks.
  const { data, isRefreshing, refresh } = useLibraryMetrics(activeLibraryId)

  const handleRefresh = useCallback(async () => {
    if (isRefreshing) return
    await refresh()
  }, [isRefreshing, refresh])
  const refreshing = isRefreshing

  // All derived values depend on `data`; useMemo stabilizes them so the
  // extracted widgets (wrapped in React.memo) only re-render when their
  // own data slice changes, not on every dashboard-level state flip.
  const derived = useMemo(() => {
    const { documentMetrics, graph, recentWebRuns, recentDocuments, attention } = data

    const totalDocuments = documentMetrics.total
    const graphReadyCount = graph.graphReadyDocumentCount
    const graphSparseCount = graph.graphSparseDocumentCount
    const failedCount = documentMetrics.failed
    const processingCount = documentMetrics.processing + documentMetrics.queued
    const readyCount = documentMetrics.ready
    const readableWithoutGraphCount = Math.max(0, readyCount - graphReadyCount - graphSparseCount)
    // In-flight is derived from the canonical mutually exclusive
    // document lifecycle buckets. It is intentionally not a separate
    // server field, so retries cannot introduce a second source of truth.
    const inFlightCount = processingCount
    const latestRun = pickLatestRun(recentWebRuns)

    const summaryCards: SummaryCard[] = [
      {
        key: 'documents',
        label: t('dashboard.total'),
        value: totalDocuments.toString(),
        detail:
          totalDocuments > 0
            ? t('dashboard.documentsReadySummary', { count: readyCount })
            : t('dashboard.noDocs'),
        icon: FileText,
        tone: 'neutral',
        actionPath: buildDocumentsPath(),
      },
      {
        key: 'in-flight',
        label: t('dashboard.inFlight'),
        value: inFlightCount.toString(),
        detail:
          inFlightCount > 0
            ? t('dashboard.inFlightSummary', { count: inFlightCount })
            : t('dashboard.pipelineIdle'),
        icon: Activity,
        tone: inFlightCount > 0 ? 'processing' : 'neutral',
        actionPath: buildDocumentsPath({ status: 'processing' }),
      },
      {
        key: 'failed',
        label: t('dashboard.failed'),
        value: failedCount.toString(),
        detail:
          failedCount > 0
            ? t('dashboard.failedSummary', { count: failedCount })
            : t('dashboard.noFailedDesc'),
        icon: XCircle,
        tone: failedCount > 0 ? 'failed' : 'ready',
        actionPath: buildDocumentsPath({ status: 'failed' }),
      },
    ]

    const healthRows: HealthRow[] = [
      {
        key: 'graph-ready',
        label: t('dashboard.graphReady'),
        count: graphReadyCount,
        className: 'bg-status-ready',
        actionPath: '/graph',
      },
      ...(readableWithoutGraphCount > 0
        ? [
            {
              key: 'readable',
              label: t('dashboard.readableNoGraph'),
              count: readableWithoutGraphCount,
              className: 'bg-status-warning',
              actionPath: '/graph',
            },
          ]
        : []),
      {
        key: 'graph-sparse',
        label: t('dashboard.graphSparse'),
        count: graphSparseCount,
        className: 'bg-status-warning',
        actionPath: '/graph',
      },
      {
        key: 'processing',
        label: t('dashboard.processing'),
        count: processingCount,
        className: 'bg-status-processing',
        actionPath: buildDocumentsPath({ status: 'processing' }),
      },
      {
        key: 'failed',
        label: t('dashboard.failed'),
        count: failedCount,
        className: 'bg-status-failed',
        actionPath: buildDocumentsPath({ status: 'failed' }),
      },
    ]

    return {
      totalDocuments,
      graphReadyCount,
      readyCount,
      readableWithoutGraphCount,
      latestRun,
      summaryCards,
      healthRows,
      graph,
      recentDocuments,
      attention,
    }
  }, [data, t])

  return (
    <PageShell
      header={
        <PageHeader
          title={t('dashboard.title')}
          description={t('dashboard.subtitle')}
          actions={
            <>
              {queryReady && (
                <Button
                  size="sm"
                  onClick={() => navigate('/assistant')}
                  className="h-8 px-3 text-xs"
                >
                  <MessageSquare className="h-4 w-4 mr-2" />
                  {t('dashboard.askAssistant')}
                </Button>
              )}
              <Button
                variant="outline"
                size="sm"
                onClick={handleRefresh}
                disabled={refreshing}
                className="h-8 px-2.5 text-xs sm:px-3"
              >
                <RefreshCw
                  className={`h-3.5 w-3.5 sm:mr-1.5 ${refreshing ? 'animate-spin' : ''}`}
                />
                <span className="sr-only sm:not-sr-only">{t('dashboard.refresh')}</span>
              </Button>
            </>
          }
        />
      }
      bodyScroll="auto"
      bodyClassName="p-3 animate-fade-in sm:p-4"
    >
      <div className="w-full space-y-4">
        <SummaryCards cards={derived.summaryCards} onNavigate={navigate} />

        <div className="grid items-stretch gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <div className="flex flex-col gap-4">
            <LibraryHealthPanel
              t={t}
              locale={i18n.language}
              graph={derived.graph}
              totalDocuments={derived.totalDocuments}
              readyCount={derived.readyCount}
              readableWithoutGraphCount={derived.readableWithoutGraphCount}
              healthRows={derived.healthRows}
              onNavigate={navigate}
            />
            <div className="flex-1">
              <RecentDocumentsList
                t={t}
                locale={i18n.language}
                recentDocuments={derived.recentDocuments}
                totalDocuments={derived.totalDocuments}
                onNavigate={navigate}
              />
            </div>
          </div>

          <div className="flex flex-col gap-4">
            <AttentionPanel t={t} attention={derived.attention} onNavigate={navigate} />
            <div className="flex-1">
              <LatestIngestPanel t={t} locale={i18n.language} latestRun={derived.latestRun} />
            </div>
          </div>
        </div>
      </div>
    </PageShell>
  )
}

export default function DashboardPage() {
  const { activeLibrary } = useApp()

  if (!activeLibrary) {
    return <DashboardEmptyState />
  }

  return (
    <Suspense fallback={<DashboardSkeleton />}>
      <DashboardContent activeLibraryId={activeLibrary.id} queryReady={activeLibrary.queryReady} />
    </Suspense>
  )
}
