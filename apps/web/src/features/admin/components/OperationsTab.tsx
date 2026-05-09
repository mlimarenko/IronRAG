import { useCallback, useEffect, useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { TFunction } from 'i18next';
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Download,
  RefreshCw,
  Search,
  Upload,
  XCircle,
} from 'lucide-react';
import { adminApi, queries } from '@/shared/api';
import { BackupExportDialog, BackupImportDialog } from './BackupDialogs';
import { DataState } from '@/shared/components/DataState';
import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { mapAuditPage, mapOps } from '@/features/admin/model/adminAdapter';
import { errorMessage } from '@/shared/lib/errorMessage';
import type {
  AuditEvent,
  AuditEventPage,
  OperationsSnapshot,
} from '@/shared/types';
import type { ListAuditEventsData } from '@/shared/api/generated';

const AUDIT_PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
const AUDIT_SURFACE_OPTIONS = ['all', 'rest', 'mcp', 'worker', 'bootstrap'] as const;
const AUDIT_RESULT_OPTIONS = ['all', 'succeeded', 'rejected', 'failed'] as const;

type AuditResultFilter = (typeof AUDIT_RESULT_OPTIONS)[number];
type AuditSurfaceFilter = (typeof AUDIT_SURFACE_OPTIONS)[number];
type AuditPageSize = (typeof AUDIT_PAGE_SIZE_OPTIONS)[number];

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
  const [exportDialogOpen, setExportDialogOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);

  const [auditSearch, setAuditSearch] = useState('');
  const [auditResultFilter, setAuditResultFilter] = useState<AuditResultFilter>('all');
  const [auditSurfaceFilter, setAuditSurfaceFilter] = useState<AuditSurfaceFilter>('all');
  const [auditPageSize, setAuditPageSize] = useState<AuditPageSize>(AUDIT_PAGE_SIZE_OPTIONS[0]);
  const [auditPage, setAuditPage] = useState(1);

  const opsQuery = useQuery({
    ...queries.getLibraryStateOptions({
      path: { libraryId: activeLibraryId ?? '' },
    }),
    enabled: active && Boolean(activeLibraryId),
  });
  const { refetch: refetchOps } = opsQuery;
  const ops = useMemo<OperationsSnapshot | null>(
    () => (opsQuery.data ? mapOps(opsQuery.data) : null),
    [opsQuery.data],
  );
  const opsLoading = opsQuery.isLoading && active && Boolean(activeLibraryId);
  const opsError = opsQuery.error
    ? errorMessage(opsQuery.error, t('admin.loadOperationsFailed'))
    : null;

  const auditQueryEnabled =
    active && Boolean(activeWorkspaceId || activeLibraryId);
  const auditQueryParams: NonNullable<ListAuditEventsData['query']> = {
    ...(activeLibraryId ? {} : activeWorkspaceId ? { workspaceId: activeWorkspaceId } : {}),
    ...(activeLibraryId ? { libraryId: activeLibraryId } : {}),
    ...(auditSearch ? { search: auditSearch } : {}),
    ...(auditSurfaceFilter === 'all' ? {} : { surfaceKind: auditSurfaceFilter }),
    ...(auditResultFilter === 'all' ? {} : { resultKind: auditResultFilter }),
    limit: auditPageSize,
    offset: (auditPage - 1) * auditPageSize,
    includeAssistant: true,
  };
  const auditQueryOptions = queries.listAuditEventsOptions({
    query: auditQueryParams,
  });
  const auditQuery = useQuery({
    ...auditQueryOptions,
    queryFn: async () => {
      const firstPage = await adminApi.listAuditEvents(auditQueryParams);
      const mappedFirstPage = mapAuditPage(firstPage);
      const totalPages = Math.max(1, Math.ceil(mappedFirstPage.total / auditPageSize));
      if (mappedFirstPage.total > 0 && auditPage > totalPages) {
        return adminApi.listAuditEvents({
          ...auditQueryParams,
          offset: (totalPages - 1) * auditPageSize,
        });
      }
      return firstPage;
    },
    enabled: auditQueryEnabled,
  });
  const { refetch: refetchAudit } = auditQuery;
  const audit = useMemo<AuditEventPage>(() => {
    if (!auditQuery.data) {
      return { items: [], total: 0, limit: auditPageSize, offset: 0 };
    }
    return mapAuditPage(auditQuery.data);
  }, [auditQuery.data, auditPageSize]);
  const auditLoading = auditQuery.isLoading && auditQueryEnabled;

  const loadOps = useCallback(() => {
    void refetchOps();
  }, [refetchOps]);
  const loadAudit = useCallback(() => {
    void refetchAudit();
  }, [refetchAudit]);

  useEffect(() => {
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) {
        setAuditPage(1);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [activeLibraryId, activeWorkspaceId]);

  const opsStatusMeta = ops ? getOperationsStatusMeta(ops, t) : null;
  const auditTotalPages = Math.max(1, Math.ceil(audit.total / auditPageSize));
  const visibleAuditPage =
    audit.total === 0 ? 1 : Math.floor(audit.offset / auditPageSize) + 1;
  const auditFrom = audit.total === 0 ? 0 : audit.offset + 1;
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
            <span className={`status-badge text-[10px] ${opsStatusMeta.badgeClass}`} title={opsStatusMeta.description}>
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
        <DataState
          query={{
            isLoading: auditLoading,
            error: auditQuery.error
              ? errorMessage(auditQuery.error, t('admin.loadAuditEventsFailed'))
              : null,
            data: audit,
          }}
          emptyCheck={(auditData) => auditData.items.length === 0}
          emptyRender={
            <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground">
              {t('admin.noAuditEvents')}
            </div>
          }
        >
          {(auditData) => (
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
                    {auditData.items.map((evt) => {
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
                            <div
                              className="font-semibold text-xs leading-tight truncate max-w-md"
                              title={evt.message}
                            >
                              {evt.message.split(' | ')[0]}
                            </div>
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
                              <div className="truncate" title={assistantModels}>
                                {t('admin.auditAssistantMeta', {
                                  cost: assistantCost,
                                  count: evt.assistantCall.providerCallCount,
                                })}
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
                  <Button size="sm" variant="outline" className="h-7 text-xs" disabled={visibleAuditPage <= 1} onClick={() => setAuditPage(Math.max(1, visibleAuditPage - 1))}>
                    {t('admin.previous')}
                  </Button>
                  <span className="text-xs text-muted-foreground min-w-20 text-center">
                    {t('admin.auditPageLabel', { page: visibleAuditPage, total: auditTotalPages })}
                  </span>
                  <Button size="sm" variant="outline" className="h-7 text-xs" disabled={visibleAuditPage >= auditTotalPages} onClick={() => setAuditPage(Math.min(auditTotalPages, visibleAuditPage + 1))}>
                    {t('admin.next')}
                  </Button>
                </div>
              </div>
            </>
          )}
        </DataState>
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
