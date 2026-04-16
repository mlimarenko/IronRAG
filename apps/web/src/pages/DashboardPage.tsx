import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import {
  Activity,
  BarChart3,
  FileText,
  Loader2,
  RefreshCw,
  Share2,
  XCircle,
} from 'lucide-react';

import { dashboardApi } from '@/api';
import { Button } from '@/components/ui/button';
import { useApp } from '@/contexts/AppContext';
import { errorMessage } from '@/lib/errorMessage';

import { SummaryCards, type SummaryCard } from './dashboard/SummaryCards';
import { LibraryHealthPanel, type HealthRow } from './dashboard/LibraryHealthPanel';
import { RecentDocumentsList } from './dashboard/RecentDocumentsList';
import { AttentionPanel } from './dashboard/AttentionPanel';
import { LatestIngestPanel } from './dashboard/LatestIngestPanel';
import { metricValue } from './dashboard/format';
import type { DashboardData, DashboardState, RecentWebRun } from './dashboard/types';
import { buildDocumentsPath } from './dashboard/types';

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

export default function DashboardPage() {
  const { t, i18n } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  const [state, setState] = useState<DashboardState>('loading');
  const [refreshing, setRefreshing] = useState(false);
  const [data, setData] = useState<DashboardData | null>(null);
  const [loadError, setLoadError] = useState('');

  const fetchDashboard = useCallback(
    async (libraryId: string, mode: 'initial' | 'refresh') => {
      try {
        const result = await dashboardApi.getLibraryDashboard(libraryId);
        setData(result as DashboardData);
        setState('loaded');
        setLoadError('');
      } catch (err: unknown) {
        if (mode === 'initial') setState('error');
        setLoadError(errorMessage(err, 'Failed to load dashboard'));
      }
    },
    [],
  );

  useEffect(() => {
    if (!activeLibrary) {
      setState('no-library');
      return;
    }
    setState('loading');
    void fetchDashboard(activeLibrary.id, 'initial');
  }, [activeLibrary, fetchDashboard]);

  const handleRefresh = useCallback(async () => {
    if (!activeLibrary || refreshing) return;
    setRefreshing(true);
    try {
      await fetchDashboard(activeLibrary.id, 'refresh');
    } finally {
      setRefreshing(false);
    }
  }, [activeLibrary, fetchDashboard, refreshing]);

  // All derived values depend on `data`; useMemo stabilizes them so the
  // extracted widgets (wrapped in React.memo) only re-render when their
  // own data slice changes, not on every dashboard-level state flip.
  const derived = useMemo(() => {
    if (!data) return null;

    const { overview, graph, recentWebRuns, metrics, recentDocuments, attention } = data;

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
    const inFlightCount = metricValue(metrics, 'in_flight', processingCount);
    const graphReadyPct =
      totalDocuments > 0
        ? Math.min(100, Math.round((graphReadyCount / totalDocuments) * 100))
        : 0;
    const graphCoverageActionPath =
      graphSparseCount > 0 || readableWithoutGraphCount > 0 ? buildDocumentsPath() : '/graph';
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
        actionPath: graphCoverageActionPath,
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
        actionPath: buildDocumentsPath(),
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
        actionPath: buildDocumentsPath(),
      },
    ];

    const healthRows: HealthRow[] = [
      {
        key: 'graph-ready',
        label: t('dashboard.graphReady'),
        count: graphReadyCount,
        className: 'bg-status-ready',
        actionPath: buildDocumentsPath(),
      },
      ...(readableWithoutGraphCount > 0
        ? [
            {
              key: 'readable',
              label: t('dashboard.readableNoGraph'),
              count: readableWithoutGraphCount,
              className: 'bg-status-warning',
              actionPath: buildDocumentsPath(),
            },
          ]
        : []),
      {
        key: 'graph-sparse',
        label: t('dashboard.graphSparse'),
        count: graphSparseCount,
        className: 'bg-status-warning',
        actionPath: buildDocumentsPath(),
      },
      {
        key: 'processing',
        label: t('dashboard.processing'),
        count: processingCount,
        className: 'bg-status-processing',
        actionPath: buildDocumentsPath(),
      },
      {
        key: 'failed',
        label: t('dashboard.failed'),
        count: failedCount,
        className: 'bg-status-failed',
        actionPath: buildDocumentsPath(),
      },
    ];

    return {
      totalDocuments,
      graphReadyCount,
      readyCount,
      readableWithoutGraphCount,
      graphReadyPct,
      graphCoverageActionPath,
      latestRun,
      summaryCards,
      healthRows,
      graph,
      recentDocuments,
      attention,
    };
  }, [data, t]);

  if (state === 'no-library') {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <BarChart3 className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('dashboard.noLibrary')}</h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm leading-relaxed">
            {t('dashboard.noLibraryDesc')}
          </p>
        </div>
      </div>
    );
  }

  if (state === 'loading') {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
        </div>
        <div className="flex-1 flex items-center justify-center">
          <div className="flex flex-col items-center gap-3">
            <Loader2 className="h-6 w-6 animate-spin text-primary/60" />
            <span className="text-sm text-muted-foreground">
              {t('dashboard.loadingDashboard')}
            </span>
          </div>
        </div>
      </div>
    );
  }

  if (state === 'error' || !derived) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header flex items-center justify-between gap-4">
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <Button variant="outline" size="sm" onClick={handleRefresh} disabled={refreshing}>
            <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${refreshing ? 'animate-spin' : ''}`} />
            {t('dashboard.retry')}
          </Button>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-destructive/10 flex items-center justify-center mb-4">
            <XCircle className="h-7 w-7 text-destructive" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('dashboard.failedToLoad')}</h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm leading-relaxed">
            {loadError || t('dashboard.unexpectedError')}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-auto ambient-bg">
      <div className="page-header flex items-center justify-between gap-4 flex-wrap relative z-10">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <p className="text-sm text-muted-foreground mt-0.5">
            <span className="font-semibold text-foreground">{activeLibrary?.name}</span>
            <span className="mx-2 text-border">·</span>
            {t('dashboard.headerSummary', {
              total: derived.totalDocuments,
              coverage: derived.graphReadyPct,
              attention: derived.attention.length,
            })}
          </p>
        </div>
        <div className="flex gap-2 flex-wrap">
          <Button variant="outline" size="sm" onClick={() => navigate('/documents')}>
            <FileText className="h-3.5 w-3.5 mr-1.5" />
            {t('dashboard.documents')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => navigate('/graph')}>
            <Share2 className="h-3.5 w-3.5 mr-1.5" />
            {t('dashboard.graph')}
          </Button>
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
            <AttentionPanel
              t={t}
              attention={derived.attention}
              graphCoverageActionPath={derived.graphCoverageActionPath}
              onNavigate={navigate}
            />
            <LatestIngestPanel
              t={t}
              locale={i18n.language}
              latestRun={derived.latestRun}
              onNavigate={navigate}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
