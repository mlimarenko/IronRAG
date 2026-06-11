import { Suspense, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import {
  Activity,
  FileText,
  MessageSquare,
  RefreshCw,
  Share2,
  XCircle,
} from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { useApp } from '@/shared/contexts/app-context';
import { useLibraryMetrics } from '@/features/dashboard/hooks/useLibraryMetrics';

import { SummaryCards, type SummaryCard } from "./components/SummaryCards";
import { LibraryHealthPanel, type HealthRow } from "./components/LibraryHealthPanel";
import { RecentDocumentsList } from "./components/RecentDocumentsList";
import { AttentionPanel } from "./components/AttentionPanel";
import { LatestIngestPanel } from "./components/LatestIngestPanel";
import { AssistantCtaPanel } from "./components/AssistantCtaPanel";
import { DashboardSkeleton } from "./components/DashboardSkeleton";
import { DashboardEmptyState } from "./components/DashboardEmptyState";
import type { RecentWebRun } from "./model/types";
import { buildDocumentsPath } from "./model/types";

function pickLatestRun(runs: RecentWebRun[]): RecentWebRun | undefined {
  let latest: RecentWebRun | undefined;
  let latestTs = -Infinity;
  for (const run of runs) {
    const ts = run.lastActivityAt ? new Date(run.lastActivityAt).getTime() : 0;
    if (ts > latestTs) {
      latestTs = ts;
      latest = run;
    }
  }
  return latest;
}

function DashboardContent({
  activeLibraryId,
  activeLibraryName,
  queryReady,
}: {
  activeLibraryId: string;
  activeLibraryName: string;
  queryReady: boolean;
}) {
  const { t, i18n } = useTranslation();
  const navigate = useNavigate();

  // Canonical live-metrics path: a shared hook polls the dashboard
  // endpoint every 2.5 s while the tab is visible, pauses on hide,
  // and fires an immediate refresh when the tab resumes. That stops
  // the "number frozen since yesterday" class of bugs — operators
  // see live-changing counts without any refresh clicks.
  const { data, isRefreshing, refresh } = useLibraryMetrics(activeLibraryId);

  const handleRefresh = useCallback(async () => {
    if (isRefreshing) return;
    await refresh();
  }, [isRefreshing, refresh]);
  const refreshing = isRefreshing;

  // All derived values depend on `data`; useMemo stabilizes them so the
  // extracted widgets (wrapped in React.memo) only re-render when their
  // own data slice changes, not on every dashboard-level state flip.
  const derived = useMemo(() => {
    const { overview, graph, recentWebRuns, recentDocuments, attention } = data;

    const totalDocuments = overview.totalDocuments;
    const graphReadyCount = graph.graphReadyDocumentCount;
    const graphSparseCount = graph.graphSparseDocumentCount;
    const failedCount = overview.failedDocuments;
    const processingCount = overview.processingDocuments;
    const readyCount = overview.readyDocuments;
    const readableWithoutGraphCount = Math.max(
      0,
      readyCount - graphReadyCount - graphSparseCount,
    );
    // `in_flight` is a derived value — `processing + queued`, both
    // already rolled into `overview.processingDocuments` by the
    // canonical aggregator on the backend. The old `metricValue(…,
    // 'in_flight', processingCount)` fallback was a two-source
    // drift trap: the `metrics[]` value came from a separate
    // queue_depth + running_attempts calculation and could diverge
    // from the document-level `processingCount` during rebuilds.
    // Read straight from the overview so dashboard numbers stay
    // internally consistent.
    const inFlightCount = processingCount;
    const graphReadyPct =
      totalDocuments > 0
        ? Math.min(100, Math.round((graphReadyCount / totalDocuments) * 100))
        : 0;
    const latestRun = pickLatestRun(recentWebRuns);

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
        key: 'graph-coverage',
        label: t('dashboard.graphCoverage'),
        value: `${graphReadyPct}%`,
        detail:
          totalDocuments > 0
            ? t('dashboard.graphCoverageSummary', {
                ready: graphReadyCount,
                total: totalDocuments,
              })
            : t('dashboard.noDocs'),
        icon: Share2,
        tone:
          graph.status === 'ready'
            ? 'ready'
            : graphReadyCount > 0
              ? 'warning'
              : 'processing',
        // Coverage is acted on in the Graph view, not Documents (DSH-01).
        actionPath: '/graph',
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
    ];

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
    ];

    return {
      totalDocuments,
      graphReadyCount,
      readyCount,
      readableWithoutGraphCount,
      graphReadyPct,
      latestRun,
      summaryCards,
      healthRows,
      graph,
      recentDocuments,
      attention,
    };
  }, [data, t]);

  return (
    <div className="flex-1 flex flex-col overflow-auto ambient-bg">
      <div className="page-header flex items-center justify-between gap-4 flex-wrap relative z-10">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <p className="text-sm text-muted-foreground mt-0.5">
            <span className="font-semibold text-foreground">{activeLibraryName}</span>
            <span className="mx-2 text-border">·</span>
            {t('dashboard.headerSummary', {
              total: derived.totalDocuments,
              coverage: derived.graphReadyPct,
              attention: derived.attention.length,
            })}
          </p>
        </div>
        <div className="flex gap-2 flex-wrap">
          {/* Primary CTA toward the product's main verb — only when the
              library is actually query-ready, otherwise it's a dead button. */}
          {queryReady && (
            <Button size="sm" onClick={() => navigate('/assistant')}>
              <MessageSquare className="h-3.5 w-3.5 mr-1.5" />
              {t('dashboard.askAssistant')}
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={handleRefresh} disabled={refreshing}>
            <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${refreshing ? 'animate-spin' : ''}`} />
            {t('dashboard.refresh')}
          </Button>
        </div>
      </div>

      <div className="flex-1 p-6 space-y-5 animate-fade-in relative z-10">
        <SummaryCards cards={derived.summaryCards} onNavigate={navigate} />

        <div className="grid items-start gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <div className="grid gap-4">
            <LibraryHealthPanel
              t={t}
              locale={i18n.language}
              graph={derived.graph}
              totalDocuments={derived.totalDocuments}
              readyCount={derived.readyCount}
              graphReadyCount={derived.graphReadyCount}
              readableWithoutGraphCount={derived.readableWithoutGraphCount}
              healthRows={derived.healthRows}
              onNavigate={navigate}
            />
            <RecentDocumentsList
              t={t}
              locale={i18n.language}
              recentDocuments={derived.recentDocuments}
              totalDocuments={derived.totalDocuments}
              onNavigate={navigate}
            />
          </div>

          <div className="grid gap-4">
            <AssistantCtaPanel
              t={t}
              libraryName={activeLibraryName}
              queryReady={queryReady}
              onNavigate={navigate}
            />
            <AttentionPanel
              t={t}
              attention={derived.attention}
              onNavigate={navigate}
            />
            <LatestIngestPanel
              t={t}
              locale={i18n.language}
              latestRun={derived.latestRun}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

export default function DashboardPage() {
  const { activeLibrary } = useApp();

  if (!activeLibrary) {
    return <DashboardEmptyState />;
  }

  return (
    <Suspense fallback={<DashboardSkeleton />}>
      <DashboardContent
        activeLibraryId={activeLibrary.id}
        activeLibraryName={activeLibrary.name}
        queryReady={activeLibrary.queryReady}
      />
    </Suspense>
  );
}
