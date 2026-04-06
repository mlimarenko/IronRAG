import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { dashboardApi } from '@/api';
import { Button } from '@/components/ui/button';
import {
  FileText, Share2, RefreshCw, AlertTriangle, CheckCircle2,
  Clock, XCircle, Loader2, ArrowRight, BarChart3, TrendingUp
} from 'lucide-react';

type DashboardState = 'no-library' | 'loading' | 'loaded' | 'error';

interface DashboardOverview {
  totalDocuments: number;
  readyDocuments: number;
  processingDocuments: number;
  failedDocuments: number;
  graphSparseDocuments: number;
}

interface DashboardMetric {
  key: string;
  label: string;
  value: string;
  level: 'info' | 'warning' | 'error';
}

interface DashboardAttentionItem {
  code: string;
  title: string;
  detail: string;
  routePath: string;
  level: 'info' | 'warning' | 'error';
}

interface RecentDocument {
  id: string;
  fileName: string;
  fileType: string;
  fileSize: number;
  uploadedAt: string;
  status: string;
  readiness: string;
}

interface DashboardGraph {
  graphReadyDocumentCount: number;
  graphSparseDocumentCount: number;
}

interface DashboardData {
  overview: DashboardOverview;
  metrics: DashboardMetric[];
  recentDocuments: RecentDocument[];
  graph: DashboardGraph;
  attention: DashboardAttentionItem[];
}

function formatRelativeTime(iso: string): string {
  const now = Date.now();
  const then = new Date(iso).getTime();
  const diffMs = now - then;
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 1) return 'just now';
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  if (diffDay === 1) return '1 day ago';
  return `${diffDay} days ago`;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function DashboardPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();
  const [state, setState] = useState<DashboardState>('loading');
  const [refreshing, setRefreshing] = useState(false);
  const [data, setData] = useState<DashboardData | null>(null);
  const [errorMessage, setErrorMessage] = useState('');

  const fetchDashboard = useCallback(async (libraryId: string) => {
    try {
      const result = await dashboardApi.getLibraryDashboard(libraryId);
      setData(result);
      setState('loaded');
      setErrorMessage('');
    } catch (err: unknown) {
      setState('error');
      setErrorMessage(err instanceof Error ? err.message : 'Failed to load dashboard');
    }
  }, []);

  useEffect(() => {
    if (!activeLibrary) { setState('no-library'); return; }
    setState('loading');
    fetchDashboard(activeLibrary.id);
  }, [activeLibrary, fetchDashboard]);

  const handleRefresh = useCallback(async () => {
    if (!activeLibrary || refreshing) return;
    setRefreshing(true);
    try {
      const result = await dashboardApi.getLibraryDashboard(activeLibrary.id);
      setData(result);
      setState('loaded');
      setErrorMessage('');
    } catch (err: unknown) {
      setErrorMessage(err instanceof Error ? err.message : 'Refresh failed');
    } finally {
      setRefreshing(false);
    }
  }, [activeLibrary, refreshing]);

  const readinessColor = (readiness: string) => {
    switch (readiness) {
      case 'graph_ready': return 'status-ready';
      case 'processing': return 'status-processing';
      case 'graph_sparse': return 'status-warning';
      case 'failed': return 'status-failed';
      case 'readable': return 'status-processing';
      default: return 'status-processing';
    }
  };

  const readinessLabel = (readiness: string) => {
    switch (readiness) {
      case 'graph_ready': return t('dashboard.graphReady');
      case 'processing': return t('dashboard.processing');
      case 'graph_sparse': return t('dashboard.graphSparse');
      case 'failed': return t('dashboard.failed');
      case 'readable': return t('dashboard.readable');
      default: return readiness;
    }
  };

  const attentionIcon = (level: string) => {
    switch (level) {
      case 'error': return XCircle;
      case 'warning': return AlertTriangle;
      default: return Clock;
    }
  };

  const attentionColor = (level: string) => {
    switch (level) {
      case 'error': return 'failed';
      case 'warning': return 'sparse';
      default: return 'processing';
    }
  };

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
          <p className="text-sm text-muted-foreground mt-2 max-w-sm leading-relaxed">{t('dashboard.noLibraryDesc')}</p>
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
            <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${refreshing ? 'animate-spin' : ''}`} /> {t('dashboard.retry')}
          </Button>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-destructive/10 flex items-center justify-center mb-4">
            <XCircle className="h-7 w-7 text-destructive" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('dashboard.failedToLoad')}</h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm leading-relaxed">{errorMessage || t('dashboard.unexpectedError')}</p>
        </div>
      </div>
    );
  }

  const { overview, graph, recentDocuments, attention } = data;

  const stats = {
    total: overview.totalDocuments,
    processing: overview.processingDocuments,
    graphReady: graph.graphReadyDocumentCount,
    graphSparse: graph.graphSparseDocumentCount,
    failed: overview.failedDocuments,
    readable: overview.readyDocuments,
  };

  const graphReadyPct = stats.total > 0 ? Math.round((stats.graphReady / stats.total) * 100) : 0;

  return (
    <div className="flex-1 flex flex-col overflow-auto ambient-bg">
      {/* Header */}
      <div className="page-header flex items-center justify-between gap-4 flex-wrap relative z-10">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <p className="text-sm text-muted-foreground mt-0.5">
            <span className="font-semibold text-foreground">{activeLibrary?.name}</span>
            <span className="mx-2 text-border">·</span>
            {stats.total} documents, {stats.graphReady} graph-ready, {stats.processing} processing
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => navigate('/documents')}>
            <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('dashboard.documents')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => navigate('/graph')}>
            <Share2 className="h-3.5 w-3.5 mr-1.5" /> {t('dashboard.graph')}
          </Button>
          <Button variant="outline" size="sm" onClick={handleRefresh} disabled={refreshing}>
            <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${refreshing ? 'animate-spin' : ''}`} /> {t('dashboard.refresh')}
          </Button>
        </div>
      </div>

      <div className="flex-1 p-6 space-y-6 animate-fade-in relative z-10">
        {/* Stats strip — premium tiles */}
        <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
          {[
            { label: t('dashboard.total'), value: stats.total, icon: FileText },
            { label: t('dashboard.graphReady'), value: stats.graphReady, icon: CheckCircle2, color: 'ready' },
            { label: t('dashboard.graphSparse'), value: stats.graphSparse, icon: AlertTriangle, color: 'sparse' },
            { label: t('dashboard.processing'), value: stats.processing, icon: Clock, color: 'processing' },
            { label: t('dashboard.readable'), value: stats.readable, icon: FileText },
            { label: t('dashboard.failed'), value: stats.failed, icon: XCircle, color: 'failed' },
          ].map(s => (
            <div key={s.label} className="stat-tile group">
              <div className="flex items-center justify-between">
                <div className={`w-8 h-8 rounded-xl flex items-center justify-center transition-transform duration-200 group-hover:scale-110`}
                  style={s.color ? {
                    background: `hsl(var(--status-${s.color}-bg))`,
                    boxShadow: `inset 0 0 0 1px hsl(var(--status-${s.color}-ring) / 0.4)`,
                  } : {
                    background: 'hsl(var(--muted))',
                  }}
                >
                  <s.icon className={`h-4 w-4 ${s.color ? `text-status-${s.color}` : 'text-muted-foreground'}`} />
                </div>
              </div>
              <div className="mt-3">
                <div className="text-2xl font-bold tracking-tight tabular-nums">{s.value}</div>
                <div className="text-[11px] font-semibold text-muted-foreground mt-0.5 uppercase tracking-wider">{s.label}</div>
              </div>
            </div>
          ))}
        </div>

        <div className="grid lg:grid-cols-3 gap-4">
          {/* Status distribution */}
          <div className="workbench-surface p-5 lg:col-span-1">
            <div className="flex items-center justify-between mb-5">
              <h3 className="text-sm font-bold tracking-tight">{t('dashboard.statusDistribution')}</h3>
              <span className="text-xs font-bold text-status-ready tabular-nums">{graphReadyPct}% {t('dashboard.ready')}</span>
            </div>
            <div className="space-y-4">
              {[
                { label: t('dashboard.graphReady'), count: stats.graphReady, pct: stats.total > 0 ? (stats.graphReady / stats.total) * 100 : 0, color: 'ready' },
                { label: t('dashboard.graphSparse'), count: stats.graphSparse, pct: stats.total > 0 ? (stats.graphSparse / stats.total) * 100 : 0, color: 'sparse' },
                { label: t('dashboard.processing'), count: stats.processing, pct: stats.total > 0 ? (stats.processing / stats.total) * 100 : 0, color: 'processing' },
                { label: t('dashboard.failed'), count: stats.failed, pct: stats.total > 0 ? (stats.failed / stats.total) * 100 : 0, color: 'failed' },
              ].map(d => (
                <div key={d.label}>
                  <div className="flex justify-between text-xs mb-2">
                    <span className="font-semibold">{d.label}</span>
                    <span className="text-muted-foreground tabular-nums font-medium">{d.count}</span>
                  </div>
                  <div className="h-2 bg-surface-sunken rounded-full overflow-hidden" style={{
                    boxShadow: 'inset 0 1px 2px hsl(var(--foreground) / 0.04)',
                  }}>
                    <div
                      className="h-full rounded-full transition-all duration-700 ease-out"
                      style={{
                        width: `${d.pct}%`,
                        background: `hsl(var(--status-${d.color}))`,
                        boxShadow: `0 0 8px -2px hsl(var(--status-${d.color}) / 0.4)`,
                      }}
                    />
                  </div>
                </div>
              ))}
            </div>
          </div>

          {/* Attention items */}
          <div className="workbench-surface p-5 lg:col-span-1">
            <h3 className="text-sm font-bold tracking-tight mb-4">{t('dashboard.attentionRequired')}</h3>
            <div className="space-y-1.5">
              {attention.length > 0 ? attention.map(item => {
                const Icon = attentionIcon(item.level);
                const color = attentionColor(item.level);
                return (
                  <button key={item.code} onClick={() => navigate(item.routePath)} className="w-full flex items-center gap-3 p-3 rounded-xl hover:bg-accent/50 transition-all duration-200 text-left group hover:shadow-soft">
                    <div className="w-8 h-8 rounded-xl flex items-center justify-center shrink-0" style={{
                      background: `hsl(var(--status-${color}-bg))`,
                      boxShadow: `inset 0 0 0 1px hsl(var(--status-${color}-ring) / 0.3)`,
                    }}>
                      <Icon className={`h-4 w-4 text-status-${color}`} />
                    </div>
                    <div className="flex-1">
                      <span className="text-sm font-semibold">{item.title}</span>
                      <div className="text-[11px] text-muted-foreground mt-0.5">{item.detail}</div>
                    </div>
                    <ArrowRight className="h-3.5 w-3.5 text-muted-foreground opacity-0 group-hover:opacity-100 group-hover:translate-x-0.5 transition-all duration-200" />
                  </button>
                );
              }) : (
                <div className="flex items-center gap-3 p-3 text-sm text-muted-foreground">
                  <div className="w-8 h-8 rounded-xl flex items-center justify-center" style={{
                    background: 'hsl(var(--status-ready-bg))',
                    boxShadow: 'inset 0 0 0 1px hsl(var(--status-ready-ring) / 0.3)',
                  }}>
                    <CheckCircle2 className="h-4 w-4 text-status-ready" />
                  </div>
                  {t('dashboard.allHealthy')}
                </div>
              )}
            </div>
          </div>

          {/* Recent documents */}
          <div className="workbench-surface p-5 lg:col-span-1">
            <h3 className="text-sm font-bold tracking-tight mb-4">{t('dashboard.recentDocs')}</h3>
            <div className="space-y-0.5">
              {recentDocuments.length > 0 ? recentDocuments.map(doc => (
                <button key={doc.id} onClick={() => navigate('/documents')} className="w-full flex items-center gap-3 p-2.5 rounded-xl hover:bg-accent/50 transition-all duration-200 text-left group hover:shadow-soft">
                  <div className="w-8 h-8 rounded-lg bg-surface-sunken flex items-center justify-center shrink-0">
                    <FileText className="h-3.5 w-3.5 text-muted-foreground" />
                  </div>
                  <div className="flex-1 min-w-0">
                    <span className="text-sm font-medium truncate block">{doc.fileName}</span>
                    <span className="text-[11px] text-muted-foreground">{formatRelativeTime(doc.uploadedAt)} · {formatSize(doc.fileSize)}</span>
                  </div>
                  <span className={`status-badge text-[10px] ${readinessColor(doc.readiness)}`}>{readinessLabel(doc.readiness)}</span>
                </button>
              )) : (
                <div className="text-sm text-muted-foreground p-3">{t('dashboard.noDocs')}</div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
