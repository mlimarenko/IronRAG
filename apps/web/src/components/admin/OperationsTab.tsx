import { useCallback, useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { useNavigate } from 'react-router-dom';
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Download,
  ExternalLink,
  Loader2,
  RefreshCw,
  Search,
  Upload,
  XCircle,
} from 'lucide-react';
import { adminApi, dashboardApi } from '@/api';
import { BackupExportDialog, BackupImportDialog } from './BackupDialogs';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { mapAuditPage, mapOps } from '@/adapters/admin';
import { errorMessage } from '@/lib/errorMessage';
import type {
  AuditEvent,
  AuditEventPage,
  OperationsSnapshot,
} from '@/types';

const AUDIT_PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
const AUDIT_SURFACE_OPTIONS = ['all', 'rest', 'mcp', 'worker', 'bootstrap'] as const;
const AUDIT_RESULT_OPTIONS = ['all', 'succeeded', 'rejected', 'failed'] as const;

type AuditResultFilter = (typeof AUDIT_RESULT_OPTIONS)[number];
type AuditSurfaceFilter = (typeof AUDIT_SURFACE_OPTIONS)[number];
type AuditPageSize = (typeof AUDIT_PAGE_SIZE_OPTIONS)[number];

type OperationsActionItemTone = 'ready' | 'warning' | 'failed';

type OperationsActionItem = {
  key: string;
  tone: OperationsActionItemTone;
  title: string;
  detail: string;
  actionLabel?: string;
  actionPath?: string;
};

type OperationsStatusMeta = {
  label: string;
  badgeClass: string;
  description: string;
};

type OperationsTabProps = {
  t: TFunction;
  activeWorkspaceId: string | undefined;
  activeLibraryId: string | undefined;
  active: boolean;
};

function buildDocumentsPath(params: Record<string, string | null | undefined> = {}): string {
  const searchParams = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value) searchParams.set(key, value);
  }
  const query = searchParams.toString();
  return query ? `/documents?${query}` : '/documents';
}

function getOperationsStatusMeta(ops: OperationsSnapshot, t: TFunction): OperationsStatusMeta {
  if (
    ops.status === 'healthy' &&
    ops.readableDocCount === 0 &&
    ops.failedDocCount === 0 &&
    ops.queueDepth === 0 &&
    ops.runningAttempts === 0
  ) {
    return {
      label: t('admin.opsStatusLabels.healthy'),
      badgeClass: 'status-ready',
      description: t('admin.opsStatusDescriptions.empty'),
    };
  }

  switch (ops.status) {
    case 'processing':
      return {
        label: t('admin.opsStatusLabels.processing'),
        badgeClass: 'status-processing',
        description: t('admin.opsStatusDescriptions.processing'),
      };
    case 'rebuilding':
      return {
        label: t('admin.opsStatusLabels.rebuilding'),
        badgeClass: 'status-warning',
        description: t('admin.opsStatusDescriptions.rebuilding'),
      };
    case 'degraded':
      return {
        label: t('admin.opsStatusLabels.degraded'),
        badgeClass: 'status-failed',
        description: t('admin.opsStatusDescriptions.degraded'),
      };
    default:
      return {
        label: t('admin.opsStatusLabels.healthy'),
        badgeClass: 'status-ready',
        description: t('admin.opsStatusDescriptions.healthy'),
      };
  }
}

function getOperationsActionItems(ops: OperationsSnapshot, t: TFunction): OperationsActionItem[] {
  const items: OperationsActionItem[] = [];

  if (ops.failedDocCount > 0) {
    items.push({
      key: 'failed_documents',
      tone: 'failed',
      title: t('admin.opsActions.failedDocuments.title'),
      detail: t('admin.opsActions.failedDocuments.detail', { count: ops.failedDocCount }),
      actionLabel: t('admin.opsActions.failedDocuments.action'),
      actionPath: buildDocumentsPath({ status: 'failed' }),
    });
  }

  const queuedOrRunning = ops.queueDepth + ops.runningAttempts;
  if (queuedOrRunning > 0) {
    items.push({
      key: 'processing_queue',
      tone: 'warning',
      title: t('admin.opsActions.processingQueue.title'),
      detail: t('admin.opsActions.processingQueue.detail', { count: queuedOrRunning }),
      actionLabel: t('admin.opsActions.processingQueue.action'),
      actionPath: `${buildDocumentsPath()}?status=processing`,
    });
  }

  for (const warning of ops.warnings) {
    switch (warning.warningKind) {
      case 'stale_vectors':
        items.push({
          key: warning.warningKind,
          tone: 'warning',
          title: t('admin.opsActions.staleVectors.title'),
          detail: t('admin.opsActions.staleVectors.detail'),
          actionLabel: t('admin.opsActions.staleVectors.action'),
          actionPath: `${buildDocumentsPath()}?status=processing`,
        });
        break;
      case 'stale_relations':
        items.push({
          key: warning.warningKind,
          tone: 'warning',
          title: t('admin.opsActions.staleRelations.title'),
          detail: t('admin.opsActions.staleRelations.detail'),
          actionLabel: t('admin.opsActions.staleRelations.action'),
          actionPath: '/graph',
        });
        break;
      case 'failed_rebuilds':
        items.push({
          key: warning.warningKind,
          tone: 'failed',
          title: t('admin.opsActions.failedRebuilds.title'),
          detail: t('admin.opsActions.failedRebuilds.detail'),
          actionLabel: t('admin.opsActions.failedRebuilds.action'),
          actionPath: buildDocumentsPath({ status: 'failed' }),
        });
        break;
      case 'bundle_assembly_failures':
        items.push({
          key: warning.warningKind,
          tone: 'failed',
          title: t('admin.opsActions.bundleFailures.title'),
          detail: t('admin.opsActions.bundleFailures.detail'),
          actionLabel: t('admin.opsActions.bundleFailures.action'),
          actionPath: '/graph',
        });
        break;
      default:
        break;
    }
  }

  const deduped = new Map<string, OperationsActionItem>();
  for (const item of items) {
    deduped.set(item.key, item);
  }

  return Array.from(deduped.values()).sort((left, right) => {
    const priority = (tone: OperationsActionItemTone) =>
      tone === 'failed' ? 2 : tone === 'warning' ? 1 : 0;
    return priority(right.tone) - priority(left.tone);
  });
}

function getOperationsActionToneClass(tone: OperationsActionItemTone): string {
  if (tone === 'failed') return 'text-status-failed border-status-failed/15 bg-status-failed/5';
  if (tone === 'warning') return 'text-status-warning border-status-warning/15 bg-status-warning/5';
  return 'text-status-ready border-status-ready/15 bg-status-ready/5';
}

function getAuditResultBadgeClass(resultKind: AuditEvent['resultKind']): string {
  if (resultKind === 'failed') return 'status-failed';
  if (resultKind === 'rejected') return 'status-warning';
  return 'status-ready';
}

function getAuditResultIcon(resultKind: AuditEvent['resultKind']) {
  if (resultKind === 'failed') return XCircle;
  if (resultKind === 'rejected') return AlertTriangle;
  return CheckCircle2;
}

function humanizeGenerationState(state: string, t: TFunction): string {
  switch (state) {
    case 'graph_ready':
      return t('admin.opsGenerationStates.graph_ready');
    case 'vector_ready':
      return t('admin.opsGenerationStates.vector_ready');
    case 'text_readable':
      return t('admin.opsGenerationStates.text_readable');
    case 'accepted':
    case 'unknown':
      return t('admin.opsGenerationStates.unknown');
    default:
      return state;
  }
}

function humanizeAuditSurface(surfaceKind: string, t: TFunction): string {
  switch (surfaceKind) {
    case 'mcp':
    case 'worker':
    case 'bootstrap':
    case 'rest':
      return t(`admin.auditSurfaceLabels.${surfaceKind}`);
    default:
      return surfaceKind;
  }
}

function humanizeAuditResult(resultKind: AuditEvent['resultKind'], t: TFunction): string {
  return t(`admin.auditResultLabels.${resultKind}`);
}

function formatAuditAssistantModels(event: AuditEvent, t: TFunction): string {
  const assistantCall = event.assistantCall;
  if (!assistantCall || assistantCall.models.length === 0) {
    return t('admin.auditAssistantNoModel');
  }
  return assistantCall.models
    .map((model) => `${model.providerKind}:${model.modelName}`)
    .join(', ');
}

function formatAuditAssistantCost(event: AuditEvent, t: TFunction): string {
  const assistantCall = event.assistantCall;
  if (!assistantCall || assistantCall.totalCost == null) {
    return t('admin.auditAssistantCostUnavailable');
  }
  return `$${Number(assistantCall.totalCost).toFixed(4)}`;
}

export function OperationsTab({
  t,
  activeWorkspaceId,
  activeLibraryId,
  active,
}: OperationsTabProps) {
  const navigate = useNavigate();

  const [ops, setOps] = useState<OperationsSnapshot | null>(null);
  const [opsLoading, setOpsLoading] = useState(false);
  const [opsError, setOpsError] = useState<string | null>(null);
  const [exportDialogOpen, setExportDialogOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);

  const [audit, setAudit] = useState<AuditEventPage>({
    items: [],
    total: 0,
    limit: AUDIT_PAGE_SIZE_OPTIONS[0],
    offset: 0,
  });
  const [auditLoading, setAuditLoading] = useState(false);
  const [auditSearch, setAuditSearch] = useState('');
  const [auditResultFilter, setAuditResultFilter] = useState<AuditResultFilter>('all');
  const [auditSurfaceFilter, setAuditSurfaceFilter] = useState<AuditSurfaceFilter>('all');
  const [auditPageSize, setAuditPageSize] = useState<AuditPageSize>(AUDIT_PAGE_SIZE_OPTIONS[0]);
  const [auditPage, setAuditPage] = useState(1);

  const loadOps = useCallback(() => {
    if (!activeLibraryId) {
      setOps(null);
      return;
    }
    setOpsLoading(true);
    setOpsError(null);
    dashboardApi
      .getLibraryState(activeLibraryId)
      .then((data) => setOps(mapOps(data)))
      .catch((err: unknown) =>
        setOpsError(errorMessage(err, t('admin.loadOperationsFailed'))),
      )
      .finally(() => setOpsLoading(false));
  }, [activeLibraryId, t]);

  const loadAudit = useCallback(() => {
    if (!activeWorkspaceId && !activeLibraryId) {
      setAudit({ items: [], total: 0, limit: auditPageSize, offset: 0 });
      return;
    }

    setAuditLoading(true);
    adminApi
      .listAuditEvents({
        workspaceId: activeLibraryId ? undefined : activeWorkspaceId,
        libraryId: activeLibraryId,
        search: auditSearch || undefined,
        surfaceKind: auditSurfaceFilter === 'all' ? undefined : auditSurfaceFilter,
        resultKind: auditResultFilter === 'all' ? undefined : auditResultFilter,
        limit: auditPageSize,
        offset: (auditPage - 1) * auditPageSize,
        includeAssistant: true,
      })
      .then((data) => {
        const pageData = mapAuditPage(data);
        const totalPages = Math.max(1, Math.ceil(pageData.total / auditPageSize));
        if (pageData.total > 0 && auditPage > totalPages) {
          setAuditPage(totalPages);
          return;
        }
        setAudit(pageData);
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.loadAuditEventsFailed'))))
      .finally(() => setAuditLoading(false));
  }, [
    activeLibraryId,
    activeWorkspaceId,
    auditPage,
    auditPageSize,
    auditResultFilter,
    auditSearch,
    auditSurfaceFilter,
    t,
  ]);

  useEffect(() => {
    if (active) {
      loadOps();
      loadAudit();
    }
  }, [active, loadOps, loadAudit]);

  useEffect(() => {
    setAuditPage(1);
  }, [activeLibraryId, activeWorkspaceId]);

  const opsStatusMeta = ops ? getOperationsStatusMeta(ops, t) : null;
  const opsActionItems = ops ? getOperationsActionItems(ops, t) : [];
  const auditTotalPages = Math.max(1, Math.ceil(audit.total / auditPageSize));
  const auditFrom = audit.total === 0 ? 0 : (auditPage - 1) * auditPageSize + 1;
  const auditTo =
    audit.total === 0 ? 0 : Math.min(audit.total, auditFrom + audit.items.length - 1);

  return (
    <div className="flex flex-col h-full min-h-0">
      {/* ── Header bar ── */}
      <div className="mb-4 flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between shrink-0">
        <div className="flex items-center gap-3">
          <h2 className="text-base font-bold tracking-tight flex items-center gap-2">
            <Activity className="h-4 w-4 text-muted-foreground" />
            {t('admin.operations')}
          </h2>
          {opsStatusMeta && (
            <span className={`status-badge ${opsStatusMeta.badgeClass}`}>
              {opsStatusMeta.label}
            </span>
          )}
          {opsError && <span className="text-xs text-status-failed">{opsError}</span>}
        </div>
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => { loadOps(); loadAudit(); }}
          >
            <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${opsLoading || auditLoading ? 'animate-spin' : ''}`} />
            {t('dashboard.refresh')}
          </Button>
          <Button size="sm" variant="outline" disabled={!activeLibraryId} onClick={() => setExportDialogOpen(true)}>
            <Download className="h-3.5 w-3.5 mr-1.5" />{t('admin.snapshot.export')}
          </Button>
          <Button size="sm" variant="outline" disabled={!activeLibraryId} onClick={() => setImportDialogOpen(true)}>
            <Upload className="h-3.5 w-3.5 mr-1.5" />{t('admin.snapshot.import')}
          </Button>
        </div>
      </div>

      {/* ── Compact status strip ── */}
      {ops ? (
        <div className="shrink-0 mb-4">
          <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
            {[
              { label: t('admin.queueDepth'), value: ops.queueDepth },
              { label: t('admin.running'), value: ops.runningAttempts },
              { label: t('admin.readableDocs'), value: ops.readableDocCount },
              { label: t('admin.failedDocs'), value: ops.failedDocCount, color: ops.failedDocCount > 0 ? 'text-status-failed' : undefined },
              { label: t('admin.knowledgeGeneration'), value: humanizeGenerationState(ops.knowledgeGenerationState, t), isText: true },
            ].map((s) => (
              <div key={s.label} className="stat-tile">
                <div className="section-label truncate">{s.label}</div>
                <div className={`${(s as { isText?: boolean }).isText ? 'text-sm' : 'text-2xl'} font-bold mt-1.5 tracking-tight tabular-nums ${(s as { color?: string }).color ?? ''}`}>
                  {s.value}
                </div>
              </div>
            ))}
          </div>
          {opsActionItems.length > 0 && (
            <div className="mt-3 flex flex-wrap gap-2">
              {opsActionItems.map((item) => (
                <button
                  key={item.key}
                  className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-semibold transition-colors ${getOperationsActionToneClass(item.tone)}`}
                  onClick={() => item.actionPath && navigate(item.actionPath)}
                >
                  {item.tone === 'failed' ? <XCircle className="h-3 w-3" /> : <AlertTriangle className="h-3 w-3" />}
                  {item.title}
                  {item.actionPath && <ExternalLink className="h-3 w-3 ml-1 opacity-60" />}
                </button>
              ))}
            </div>
          )}
        </div>
      ) : !opsLoading && !opsError && (
        <div className="text-sm text-muted-foreground text-center p-6 border rounded-xl bg-surface-sunken mb-4 shrink-0">
          {activeLibraryId ? t('admin.noOpsData') : t('admin.selectLibraryOps')}
        </div>
      )}

      {/* ── Audit log: filters ── */}
      <div className="shrink-0 mb-3 flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
        <h3 className="text-sm font-bold tracking-tight">{t('admin.auditLog')}</h3>
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
            <Input
              className="h-8 pl-9 w-48 text-xs"
              placeholder={t('admin.auditSearchPlaceholder')}
              value={auditSearch}
              onChange={(e) => { setAuditSearch(e.target.value); setAuditPage(1); }}
            />
          </div>
          <Select value={auditResultFilter} onValueChange={(v) => { setAuditResultFilter(v as AuditResultFilter); setAuditPage(1); }}>
            <SelectTrigger className="h-8 w-32 text-xs"><SelectValue /></SelectTrigger>
            <SelectContent>
              {AUDIT_RESULT_OPTIONS.map((o) => (
                <SelectItem key={o} value={o}>{o === 'all' ? t('admin.auditResultAll') : humanizeAuditResult(o, t)}</SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={auditSurfaceFilter} onValueChange={(v) => { setAuditSurfaceFilter(v as AuditSurfaceFilter); setAuditPage(1); }}>
            <SelectTrigger className="h-8 w-32 text-xs"><SelectValue /></SelectTrigger>
            <SelectContent>
              {AUDIT_SURFACE_OPTIONS.map((o) => (
                <SelectItem key={o} value={o}>{o === 'all' ? t('admin.auditSurfaceAll') : humanizeAuditSurface(o, t)}</SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={String(auditPageSize)} onValueChange={(v) => { setAuditPageSize(Number(v) as AuditPageSize); setAuditPage(1); }}>
            <SelectTrigger className="h-8 w-24 text-xs"><SelectValue /></SelectTrigger>
            <SelectContent>
              {AUDIT_PAGE_SIZE_OPTIONS.map((o) => (
                <SelectItem key={o} value={String(o)}>{t('admin.auditPageSizeOption', { count: o })}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      {/* ── Audit table ── */}
      <div className="flex-1 min-h-0 flex flex-col">
        {auditLoading ? (
          <div className="flex-1 flex items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" /> {t('admin.loadingAudit')}
          </div>
        ) : audit.items.length === 0 ? (
          <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground">
            {t('admin.noAuditEvents')}
          </div>
        ) : (
          <>
            <div className="flex-1 min-h-0 overflow-auto workbench-surface rounded-t-xl">
              <table className="w-full text-sm">
                <thead className="sticky top-0 bg-card z-10">
                  <tr className="border-b text-left">
                    <th className="w-8 px-3 py-2.5" />
                    <th className="px-3 py-2.5 section-label">{t('admin.auditAction')}</th>
                    <th className="px-3 py-2.5 section-label">{t('admin.auditActor')}</th>
                    <th className="px-3 py-2.5 section-label">{t('admin.auditSurface')}</th>
                    <th className="px-3 py-2.5 section-label">{t('admin.auditTime')}</th>
                    <th className="px-3 py-2.5 section-label">{t('admin.auditDetails')}</th>
                    <th className="px-3 py-2.5 section-label text-right">{t('admin.auditResult')}</th>
                  </tr>
                </thead>
                <tbody>
                  {audit.items.map((evt) => {
                    const ResultIcon = getAuditResultIcon(evt.resultKind);
                    const assistantModels = evt.assistantCall
                      ? formatAuditAssistantModels(evt, t)
                      : '';
                    const assistantCost = evt.assistantCall
                      ? formatAuditAssistantCost(evt, t)
                      : '';
                    return (
                      <tr key={evt.id} className="border-b border-border/50 hover:bg-accent/30 transition-colors">
                        <td className="px-3 py-2.5">
                          <div className={evt.resultKind === 'failed' ? 'text-status-failed' : evt.resultKind === 'rejected' ? 'text-status-warning' : 'text-status-ready'}>
                            <ResultIcon className="h-3.5 w-3.5" />
                          </div>
                        </td>
                        <td className="px-3 py-2.5">
                          <div className="font-semibold text-xs leading-tight">{evt.message}</div>
                          <div className="text-[11px] text-muted-foreground font-mono mt-0.5">{evt.action}</div>
                          {evt.assistantCall && (
                            <div className="mt-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
                              {t('admin.auditAssistantLabel')}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-2.5 text-xs text-muted-foreground font-medium whitespace-nowrap">{evt.actor}</td>
                        <td className="px-3 py-2.5 text-xs whitespace-nowrap">
                          <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide">
                            {humanizeAuditSurface(evt.surfaceKind, t)}
                          </span>
                        </td>
                        <td className="px-3 py-2.5 text-xs text-muted-foreground tabular-nums whitespace-nowrap">
                          {new Date(evt.timestamp).toLocaleString()}
                        </td>
                        <td className="px-3 py-2.5 text-xs text-muted-foreground max-w-64">
                          {evt.assistantCall ? (
                            <div className="space-y-0.5">
                              <div className="font-medium text-foreground truncate" title={assistantModels}>
                                {assistantModels}
                              </div>
                              <div className="truncate" title={assistantCost}>
                                {t('admin.auditAssistantMeta', {
                                  cost: assistantCost,
                                  count: evt.assistantCall.providerCallCount,
                                })}
                              </div>
                            </div>
                          ) : (
                            <div className="truncate" title={evt.subjectSummary ?? undefined}>
                              {evt.subjectSummary || '\u2014'}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-2.5 text-right">
                          <span className={`status-badge text-[10px] ${getAuditResultBadgeClass(evt.resultKind)}`}>
                            {humanizeAuditResult(evt.resultKind, t)}
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>

            {/* ── Pagination footer ── */}
            <div className="shrink-0 flex items-center justify-between px-4 py-2.5 border-t bg-card rounded-b-xl">
              <div className="text-xs text-muted-foreground">
                {t('admin.auditSummary', { from: auditFrom, to: auditTo, total: audit.total })}
              </div>
              <div className="flex items-center gap-2">
                <Button size="sm" variant="outline" className="h-7 text-xs" disabled={auditPage <= 1} onClick={() => setAuditPage((c) => Math.max(1, c - 1))}>
                  {t('admin.previous')}
                </Button>
                <span className="text-xs text-muted-foreground min-w-20 text-center">
                  {t('admin.auditPageLabel', { page: auditPage, total: auditTotalPages })}
                </span>
                <Button size="sm" variant="outline" className="h-7 text-xs" disabled={auditPage >= auditTotalPages} onClick={() => setAuditPage((c) => Math.min(auditTotalPages, c + 1))}>
                  {t('admin.next')}
                </Button>
              </div>
            </div>
          </>
        )}
      </div>

      {activeLibraryId && (
        <>
          <BackupExportDialog open={exportDialogOpen} onOpenChange={setExportDialogOpen} libraryId={activeLibraryId} t={t} />
          <BackupImportDialog open={importDialogOpen} onOpenChange={setImportDialogOpen} libraryId={activeLibraryId} t={t} onCompleted={loadOps} />
        </>
      )}
    </div>
  );
}
