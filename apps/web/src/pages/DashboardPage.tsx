import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import {
  Activity,
  AlertTriangle,
  ArrowRight,
  BarChart3,
  CheckCircle2,
  Clock,
  Database,
  FileText,
  Globe,
  Loader2,
  RefreshCw,
  Share2,
  XCircle,
} from 'lucide-react';

import { dashboardApi } from '@/api';
import { Button } from '@/components/ui/button';
import { useApp } from '@/contexts/AppContext';
import { humanizeDocumentFailure, humanizeDocumentStage } from '@/lib/document-processing';
import type { DocumentReadiness } from '@/types';

type DashboardState = 'no-library' | 'loading' | 'loaded' | 'error';
type MessageLevel = 'info' | 'warning' | 'error';
type GraphStatus = 'empty' | 'building' | 'rebuilding' | 'ready' | 'partial' | 'failed' | 'stale';
type WebIngestRunState =
  | 'accepted'
  | 'discovering'
  | 'processing'
  | 'completed'
  | 'completed_partial'
  | 'failed'
  | 'canceled';

interface DashboardOverview {
  totalDocuments: number;
  readyDocuments: number;
  processingDocuments: number;
  failedDocuments: number;
  graphSparseDocuments: number;
}

interface DashboardMetric {
  key: string;
  value: string;
  level: MessageLevel;
}

interface DashboardAttentionItem {
  code: string;
  title: string;
  detail: string;
  routePath: string;
  level: MessageLevel;
}

interface RecentDocument {
  id: string;
  fileName: string;
  fileSize: number;
  uploadedAt: string;
  readiness: DocumentReadiness;
  stageLabel?: string | null;
  failureMessage?: string | null;
  canRetry: boolean;
  preparedSegmentCount?: number | null;
  technicalFactCount?: number | null;
}

interface DashboardGraph {
  status: GraphStatus;
  warning?: string | null;
  nodeCount: number;
  edgeCount: number;
  graphReadyDocumentCount: number;
  graphSparseDocumentCount: number;
  typedFactDocumentCount: number;
  updatedAt?: string | null;
}

interface WebRunCounts {
  discovered: number;
  eligible: number;
  processed: number;
  queued: number;
  processing: number;
  blocked: number;
  failed: number;
}

interface RecentWebRun {
  runId: string;
  runState: WebIngestRunState;
  seedUrl: string;
  counts: WebRunCounts;
  lastActivityAt?: string | null;
}

interface DashboardData {
  overview: DashboardOverview;
  metrics: DashboardMetric[];
  recentDocuments: RecentDocument[];
  recentWebRuns: RecentWebRun[];
  graph: DashboardGraph;
  attention: DashboardAttentionItem[];
}

function formatRelativeTime(iso: string, locale: string): string {
  const timestamp = new Date(iso).getTime();
  if (Number.isNaN(timestamp)) {
    return iso;
  }

  const diffSeconds = Math.round((timestamp - Date.now()) / 1000);
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });

  if (Math.abs(diffSeconds) < 60) {
    return formatter.format(diffSeconds, 'second');
  }

  const diffMinutes = Math.round(diffSeconds / 60);
  if (Math.abs(diffMinutes) < 60) {
    return formatter.format(diffMinutes, 'minute');
  }

  const diffHours = Math.round(diffMinutes / 60);
  if (Math.abs(diffHours) < 24) {
    return formatter.format(diffHours, 'hour');
  }

  const diffDays = Math.round(diffHours / 24);
  return formatter.format(diffDays, 'day');
}

function formatDateTime(iso: string | null | undefined, locale: string, emptyLabel: string): string {
  if (!iso) {
    return emptyLabel;
  }

  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return emptyLabel;
  }

  return new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date);
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function metricValue(metrics: DashboardMetric[], key: string, fallback = 0): number {
  const value = Number(metrics.find(metric => metric.key === key)?.value ?? fallback);
  return Number.isFinite(value) ? value : fallback;
}

function readinessClass(readiness: DocumentReadiness): string {
  switch (readiness) {
    case 'graph_ready':
      return 'status-ready';
    case 'graph_sparse':
      return 'status-warning';
    case 'failed':
      return 'status-failed';
    case 'readable':
    case 'processing':
    default:
      return 'status-processing';
  }
}

function attentionClass(level: MessageLevel): string {
  switch (level) {
    case 'error':
      return 'status-failed';
    case 'warning':
      return 'status-warning';
    case 'info':
    default:
      return 'status-processing';
  }
}

function graphStatusClass(status: GraphStatus): string {
  switch (status) {
    case 'ready':
      return 'status-ready';
    case 'partial':
    case 'stale':
    case 'building':
    case 'rebuilding':
      return 'status-warning';
    case 'failed':
      return 'status-failed';
    case 'empty':
    default:
      return 'status-processing';
  }
}

function runStateClass(state: WebIngestRunState): string {
  switch (state) {
    case 'completed':
      return 'status-ready';
    case 'completed_partial':
    case 'discovering':
    case 'accepted':
    case 'processing':
      return 'status-warning';
    case 'failed':
    case 'canceled':
      return 'status-failed';
    default:
      return 'status-processing';
  }
}

function toneStyle(tone: 'neutral' | 'ready' | 'warning' | 'processing' | 'failed') {
  if (tone === 'neutral') {
    return {
      container: { background: 'hsl(var(--muted))' },
      iconClass: 'text-muted-foreground',
    };
  }

  return {
    container: {
      background: `hsl(var(--status-${tone}-bg))`,
      boxShadow: `inset 0 0 0 1px hsl(var(--status-${tone}-ring) / 0.35)`,
    },
    iconClass:
      tone === 'ready'
        ? 'text-status-ready'
        : tone === 'warning'
          ? 'text-status-warning'
          : tone === 'failed'
            ? 'text-status-failed'
            : 'text-status-processing',
  };
}

function hostnameFromUrl(value: string): string {
  try {
    return new URL(value).hostname;
  } catch {
    return value;
  }
}

function buildDocumentsPath(filters: {
  status?: 'in_progress' | 'ready' | 'failed';
  readiness?: DocumentReadiness;
  documentId?: string;
} = {}): string {
  const params = new URLSearchParams();

  if (filters.status) {
    params.set('status', filters.status);
  }
  if (filters.readiness) {
    params.set('readiness', filters.readiness);
  }
  if (filters.documentId) {
    params.set('documentId', filters.documentId);
  }

  const query = params.toString();
  return query ? `/documents?${query}` : '/documents';
}

export default function DashboardPage() {
  const { t, i18n } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  const [state, setState] = useState<DashboardState>('loading');
  const [refreshing, setRefreshing] = useState(false);
  const [data, setData] = useState<DashboardData | null>(null);
  const [errorMessage, setErrorMessage] = useState('');

  const fetchDashboard = useCallback(async (libraryId: string) => {
    try {
      const result = await dashboardApi.getLibraryDashboard(libraryId);
      setData(result as DashboardData);
      setState('loaded');
      setErrorMessage('');
    } catch (err: unknown) {
      setState('error');
      setErrorMessage(err instanceof Error ? err.message : 'Failed to load dashboard');
    }
  }, []);

  useEffect(() => {
    if (!activeLibrary) {
      setState('no-library');
      return;
    }

    setState('loading');
    void fetchDashboard(activeLibrary.id);
  }, [activeLibrary, fetchDashboard]);

  const handleRefresh = useCallback(async () => {
    if (!activeLibrary || refreshing) return;

    setRefreshing(true);
    try {
      const result = await dashboardApi.getLibraryDashboard(activeLibrary.id);
      setData(result as DashboardData);
      setState('loaded');
      setErrorMessage('');
    } catch (err: unknown) {
      setErrorMessage(err instanceof Error ? err.message : 'Refresh failed');
    } finally {
      setRefreshing(false);
    }
  }, [activeLibrary, refreshing]);

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
            <span className="text-sm text-muted-foreground">{t('dashboard.loadingDashboard')}</span>
          </div>
        </div>
      </div>
    );
  }

  if (state === 'error' || !data) {
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
            {errorMessage || t('dashboard.unexpectedError')}
          </p>
        </div>
      </div>
    );
  }

  const { overview, graph, recentDocuments, recentWebRuns, attention, metrics } = data;

  const totalDocuments = overview.totalDocuments;
  const graphReadyCount = graph.graphReadyDocumentCount;
  const graphSparseCount = graph.graphSparseDocumentCount;
  const failedCount = overview.failedDocuments;
  const processingCount = overview.processingDocuments;
  const readyCount = overview.readyDocuments;
  const readableWithoutGraphCount = Math.max(0, readyCount - graphReadyCount - graphSparseCount);
  const inFlightCount = metricValue(metrics, 'in_flight', processingCount);
  const graphReadyPct =
    totalDocuments > 0 ? Math.min(100, Math.round((graphReadyCount / totalDocuments) * 100)) : 0;
  const graphCoverageActionPath =
    graphSparseCount > 0
      ? buildDocumentsPath({ readiness: 'graph_sparse' })
      : readableWithoutGraphCount > 0
        ? buildDocumentsPath({ readiness: 'readable' })
        : '/graph';
  const latestRun = [...recentWebRuns].sort((left, right) => {
    const leftTs = left.lastActivityAt ? new Date(left.lastActivityAt).getTime() : 0;
    const rightTs = right.lastActivityAt ? new Date(right.lastActivityAt).getTime() : 0;
    return rightTs - leftTs;
  })[0];
  const emptyLabel = t('dashboard.notAvailable');
  const localizedAttention = (item: DashboardAttentionItem) => {
    switch (item.code) {
      case 'failed_documents':
      case 'graph_sparse':
      case 'graph_coverage_gap':
      case 'retryable_document':
      case 'stale_vectors':
      case 'stale_relations':
      case 'failed_rebuilds':
      case 'bundle_assembly_failures':
        return {
          title: t(`dashboard.attentionTitles.${item.code}`),
          detail: t(`dashboard.attentionDetails.${item.code}`),
        };
      default:
        return {
          title: item.title,
          detail: item.detail,
        };
    }
  };
  const attentionRoute = (item: DashboardAttentionItem) => {
    switch (item.code) {
      case 'failed_documents':
      case 'retryable_document':
      case 'failed_rebuilds':
        return buildDocumentsPath({ status: 'failed' });
      case 'stale_vectors':
        return buildDocumentsPath({ status: 'in_progress' });
      case 'graph_sparse':
        return buildDocumentsPath({ readiness: 'graph_sparse' });
      case 'graph_coverage_gap':
        return graphCoverageActionPath;
      case 'stale_relations':
      case 'bundle_assembly_failures':
        return '/graph';
      default:
        return item.routePath;
    }
  };

  const summaryCards = [
    {
      key: 'documents',
      label: t('dashboard.total'),
      value: totalDocuments.toString(),
      detail:
        totalDocuments > 0
          ? t('dashboard.documentsReadySummary', { count: readyCount })
          : t('dashboard.noDocs'),
      icon: FileText,
      tone: 'neutral' as const,
      actionPath: buildDocumentsPath(),
    },
    {
      key: 'graph-coverage',
      label: t('dashboard.graphCoverage'),
      value: `${graphReadyPct}%`,
      detail:
        totalDocuments > 0
          ? t('dashboard.graphCoverageSummary', { ready: graphReadyCount, total: totalDocuments })
          : t('dashboard.noDocs'),
      icon: Share2,
      tone: graph.status === 'ready' ? ('ready' as const) : graphReadyCount > 0 ? ('warning' as const) : ('processing' as const),
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
      tone: inFlightCount > 0 ? ('processing' as const) : ('neutral' as const),
      actionPath: buildDocumentsPath(inFlightCount > 0 ? { status: 'in_progress' } : {}),
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
      tone: failedCount > 0 ? ('failed' as const) : ('ready' as const),
      actionPath: buildDocumentsPath({ status: 'failed' }),
    },
  ];

  const healthRows = [
    {
      key: 'graph-ready',
      label: t('dashboard.graphReady'),
      count: graphReadyCount,
      className: 'bg-status-ready',
      actionPath: buildDocumentsPath({ readiness: 'graph_ready' }),
    },
    ...(readableWithoutGraphCount > 0
      ? [{
          key: 'readable',
          label: t('dashboard.readableNoGraph'),
          count: readableWithoutGraphCount,
          className: 'bg-status-warning',
          actionPath: buildDocumentsPath({ readiness: 'readable' }),
        }]
      : []),
    {
      key: 'graph-sparse',
      label: t('dashboard.graphSparse'),
      count: graphSparseCount,
      className: 'bg-status-warning',
      actionPath: buildDocumentsPath({ readiness: 'graph_sparse' }),
    },
    {
      key: 'processing',
      label: t('dashboard.processing'),
      count: processingCount,
      className: 'bg-status-processing',
      actionPath: buildDocumentsPath({ status: 'in_progress' }),
    },
    {
      key: 'failed',
      label: t('dashboard.failed'),
      count: failedCount,
      className: 'bg-status-failed',
      actionPath: buildDocumentsPath({ status: 'failed' }),
    },
  ];

  return (
    <div className="flex-1 flex flex-col overflow-auto ambient-bg">
      <div className="page-header flex items-center justify-between gap-4 flex-wrap relative z-10">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <p className="text-sm text-muted-foreground mt-0.5">
            <span className="font-semibold text-foreground">{activeLibrary?.name}</span>
            <span className="mx-2 text-border">·</span>
            {t('dashboard.headerSummary', {
              total: totalDocuments,
              coverage: graphReadyPct,
              attention: attention.length,
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
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {summaryCards.map(card => {
            const Icon = card.icon;
            const tone = toneStyle(card.tone);
            return (
              <button
                key={card.key}
                type="button"
                onClick={() => navigate(card.actionPath)}
                className="stat-tile w-full cursor-pointer text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35"
              >
                <div
                  className="w-10 h-10 rounded-xl flex items-center justify-center"
                  style={tone.container}
                >
                  <Icon className={`h-4 w-4 ${tone.iconClass}`} />
                </div>
                <div className="mt-4">
                  <div className="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider">
                    {card.label}
                  </div>
                  <div className="mt-1 text-3xl font-bold tracking-tight tabular-nums">
                    {card.value}
                  </div>
                  <div className="mt-2 text-xs leading-relaxed text-muted-foreground">
                    {card.detail}
                  </div>
                </div>
              </button>
            );
          })}
        </div>

        <div className="grid items-start gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <div className="grid gap-4">
            <div className="workbench-surface p-5 sm:p-6">
              <div className="flex items-start justify-between gap-4 flex-wrap">
                <div>
                  <h2 className="text-sm font-bold tracking-tight">{t('dashboard.libraryHealth')}</h2>
                  <p className="text-xs text-muted-foreground mt-1.5">
                    {totalDocuments > 0
                      ? t('dashboard.graphCoverageSummary', {
                          ready: graphReadyCount,
                          total: totalDocuments,
                        })
                      : t('dashboard.noDocs')}
                    {totalDocuments > 0 && (
                      <>
                        <span className="mx-1.5 text-border">·</span>
                        {t('dashboard.documentsReadySummary', { count: readyCount })}
                      </>
                    )}
                    {readableWithoutGraphCount > 0 && (
                      <>
                        <span className="mx-1.5 text-border">·</span>
                        {t('dashboard.readableNoGraphSummary', { count: readableWithoutGraphCount })}
                      </>
                    )}
                  </p>
                </div>
                <div className="flex flex-col items-start gap-2 sm:items-end">
                  <span className={`status-badge ${graphStatusClass(graph.status)}`}>
                    {t(`dashboard.graphStatusLabels.${graph.status}`)}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {t('dashboard.updated')}:{' '}
                    {formatDateTime(graph.updatedAt, i18n.language, emptyLabel)}
                  </span>
                </div>
              </div>

              <div className="mt-6 space-y-4">
                {healthRows.map(row => {
                  const ratio =
                    totalDocuments > 0
                      ? Math.min(100, Math.round((row.count / totalDocuments) * 100))
                      : 0;

                  return (
                    <button
                      key={row.key}
                      type="button"
                      onClick={() => navigate(row.actionPath)}
                      className="block w-full rounded-lg px-0.5 py-1 text-left transition-colors hover:bg-accent/25 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                    >
                      <div className="flex items-center justify-between gap-3 text-xs">
                        <span className="font-semibold text-foreground">{row.label}</span>
                        <span className="text-muted-foreground tabular-nums">
                          {row.count}
                          {totalDocuments > 0 && <span className="ml-1">{ratio}%</span>}
                        </span>
                      </div>
                      <div className="mt-2 h-2 rounded-full bg-surface-sunken overflow-hidden">
                        <div
                          className={`h-full rounded-full transition-all duration-700 ease-out ${row.className}`}
                          style={{ width: `${ratio}%` }}
                        />
                      </div>
                    </button>
                  );
                })}
              </div>

              <div className="mt-6 grid grid-cols-3 gap-3">
                {[
                  { label: t('dashboard.nodes'), value: graph.nodeCount, icon: Share2 },
                  { label: t('dashboard.edges'), value: graph.edgeCount, icon: Activity },
                  { label: t('dashboard.factDocs'), value: graph.typedFactDocumentCount, icon: Database },
                ].map(item => (
                  <div key={item.label} className="rounded-xl border border-border/60 bg-background/70 p-3.5">
                    <div className="flex items-center gap-2 text-muted-foreground">
                      <item.icon className="h-3.5 w-3.5" />
                      <span className="text-[11px] font-semibold uppercase tracking-wider">
                        {item.label}
                      </span>
                    </div>
                    <div className="mt-2 text-xl font-bold tracking-tight tabular-nums">
                      {item.value}
                    </div>
                  </div>
                ))}
              </div>
            </div>

            <div className="workbench-surface p-5 sm:p-6">
              <div className="flex items-center justify-between gap-3 flex-wrap">
                <div>
                  <h2 className="text-sm font-bold tracking-tight">{t('dashboard.recentDocs')}</h2>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {t('dashboard.recentDocsSummary', {
                      count: recentDocuments.length,
                      total: totalDocuments,
                    })}
                  </p>
                </div>
                <Button variant="outline" size="sm" onClick={() => navigate('/documents')}>
                  <FileText className="h-3.5 w-3.5 mr-1.5" />
                  {t('dashboard.openDocuments')}
                </Button>
              </div>

              {recentDocuments.length > 0 ? (
                <div className="mt-4 grid gap-3 xl:grid-cols-2">
                  {recentDocuments.map(doc => {
                    const detailBits = [];

                    if (doc.readiness === 'failed' && doc.failureMessage) {
                      detailBits.push(
                        humanizeDocumentFailure({
                          failureCode: doc.failureMessage,
                          stalledReason: doc.failureMessage,
                          stage: doc.stageLabel,
                        }, t) ?? doc.failureMessage,
                      );
                    } else if (doc.readiness === 'processing' && doc.stageLabel) {
                      detailBits.push(humanizeDocumentStage(doc.stageLabel, t) ?? doc.stageLabel);
                    } else {
                      if ((doc.preparedSegmentCount ?? 0) > 0) {
                        detailBits.push(
                          t('dashboard.segmentsSummary', { count: doc.preparedSegmentCount ?? 0 }),
                        );
                      }
                      if ((doc.technicalFactCount ?? 0) > 0) {
                        detailBits.push(
                          t('dashboard.factsSummary', { count: doc.technicalFactCount ?? 0 }),
                        );
                      }
                    }

                    if (doc.canRetry) {
                      detailBits.push(t('dashboard.retryAvailable'));
                    }

                    return (
                      <button
                        key={doc.id}
                        type="button"
                        onClick={() => navigate(buildDocumentsPath({ documentId: doc.id }))}
                        className="w-full rounded-xl border border-border/60 bg-background/70 p-3.5 text-left transition-colors hover:bg-accent/45 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                      >
                        <div className="flex items-start gap-3">
                          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-surface-sunken">
                            <FileText className="h-4 w-4 text-muted-foreground" />
                          </div>
                          <div className="min-w-0 flex-1">
                            <div className="flex items-start justify-between gap-3">
                              <div className="min-w-0">
                                <div className="truncate text-sm font-semibold text-foreground">
                                  {doc.fileName}
                                </div>
                                <div className="mt-1 text-[11px] text-muted-foreground">
                                  {formatRelativeTime(doc.uploadedAt, i18n.language)}
                                  <span className="mx-1 text-border">·</span>
                                  {formatSize(doc.fileSize)}
                                </div>
                              </div>
                              <span className={`status-badge shrink-0 text-[10px] ${readinessClass(doc.readiness)}`}>
                                {t(`dashboard.readinessLabels.${doc.readiness}`)}
                              </span>
                            </div>

                            {detailBits.length > 0 ? (
                              <div
                                className={`mt-2 text-[11px] leading-relaxed ${
                                  doc.readiness === 'failed' ? 'text-status-failed' : 'text-muted-foreground'
                                }`}
                              >
                                {detailBits.join(' · ')}
                              </div>
                            ) : null}
                          </div>
                        </div>
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="mt-4 rounded-xl border border-dashed border-border/70 bg-background/60 p-4 text-sm text-muted-foreground">
                  {t('dashboard.noDocs')}
                </div>
              )}
            </div>
          </div>

          <div className="grid gap-4">
            <div className="workbench-surface p-5 sm:p-6">
              <div className="flex items-center justify-between gap-3">
                <h2 className="text-sm font-bold tracking-tight">{t('dashboard.attentionRequired')}</h2>
                <span className={`status-badge ${attention.length > 0 ? 'status-failed' : 'status-ready'}`}>
                  {attention.length}
                </span>
              </div>

              {attention.length > 0 ? (
                <div className="mt-4 space-y-2">
                  {attention.map(item => {
                    const content = localizedAttention(item);

                    return (
                      <button
                        key={item.code}
                        type="button"
                        onClick={() => navigate(attentionRoute(item))}
                        className="w-full rounded-xl border border-border/60 bg-background/70 p-3.5 text-left transition-colors hover:bg-accent/45 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                      >
                        <div className="flex items-start gap-3">
                          <div
                            className={`mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl ${attentionClass(item.level)}`}
                          >
                            {item.level === 'error' ? (
                              <XCircle className="h-4 w-4" />
                            ) : item.level === 'warning' ? (
                              <AlertTriangle className="h-4 w-4" />
                            ) : (
                              <Clock className="h-4 w-4" />
                            )}
                          </div>
                          <div className="min-w-0 flex-1">
                            <div className="flex items-start justify-between gap-3">
                              <span className="text-sm font-semibold text-foreground">
                                {content.title}
                              </span>
                              <ArrowRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                            </div>
                            <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                              {content.detail}
                            </p>
                          </div>
                        </div>
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="mt-4 rounded-xl border border-border/60 bg-background/70 p-4">
                  <div className="flex items-center gap-3">
                    <div className="flex h-9 w-9 items-center justify-center rounded-xl status-ready">
                      <CheckCircle2 className="h-4 w-4" />
                    </div>
                    <div>
                      <div className="text-sm font-semibold text-foreground">{t('dashboard.allHealthy')}</div>
                      <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                        {t('dashboard.noAttentionDesc')}
                      </p>
                    </div>
                  </div>
                </div>
              )}
            </div>

            <div className="workbench-surface p-5 sm:p-6">
              <div className="flex items-center justify-between gap-3">
                <h2 className="text-sm font-bold tracking-tight">{t('dashboard.latestIngest')}</h2>
                {latestRun ? (
                  <span className={`status-badge ${runStateClass(latestRun.runState)}`}>
                    {t(`dashboard.runStateLabels.${latestRun.runState}`)}
                  </span>
                ) : null}
              </div>

              {latestRun ? (
                <>
                  <div className="mt-4">
                    <div className="flex items-center gap-2 text-sm font-semibold text-foreground">
                      <Globe className="h-4 w-4 text-muted-foreground" />
                      <span className="truncate">{hostnameFromUrl(latestRun.seedUrl)}</span>
                    </div>
                    <div className="mt-1 truncate text-xs text-muted-foreground">{latestRun.seedUrl}</div>
                  </div>

                  <div className="mt-4 grid grid-cols-3 gap-3">
                    {[
                      {
                        label: t('dashboard.processed'),
                        value: latestRun.counts.processed,
                        className: 'text-status-ready',
                      },
                      {
                        label: t('dashboard.queued'),
                        value: latestRun.counts.queued + latestRun.counts.processing,
                        className: 'text-status-processing',
                      },
                      {
                        label: t('dashboard.failed'),
                        value: latestRun.counts.failed + latestRun.counts.blocked,
                        className: 'text-status-failed',
                      },
                    ].map(item => (
                      <div key={item.label} className="rounded-xl border border-border/60 bg-background/70 p-3">
                        <div className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                          {item.label}
                        </div>
                        <div className={`mt-2 text-xl font-bold tracking-tight tabular-nums ${item.className}`}>
                          {item.value}
                        </div>
                      </div>
                    ))}
                  </div>

                  <div className="mt-4 flex items-center justify-between gap-3 text-xs text-muted-foreground">
                    <span>{t('dashboard.lastActivity')}</span>
                    <span className="text-right">
                      {formatDateTime(latestRun.lastActivityAt, i18n.language, emptyLabel)}
                    </span>
                  </div>

                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-4 w-full justify-between"
                    onClick={() => navigate('/documents')}
                  >
                    {t('dashboard.openDocuments')}
                    <ArrowRight className="h-3.5 w-3.5" />
                  </Button>
                </>
              ) : (
                <div className="mt-4 rounded-xl border border-dashed border-border/70 bg-background/60 p-4 text-sm text-muted-foreground">
                  {t('dashboard.noRecentRuns')}
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
