import type { TFunction } from 'i18next';
import {
  FilePenLine,
  Download,
  ExternalLink,
  Globe,
  Loader2,
  RotateCw,
  Trash2,
  Upload,
  X,
  XCircle,
} from 'lucide-react';

import { Button } from '@/components/ui/button';
import type { DocumentItem, DocumentLifecycle, DocumentReadiness } from '@/types';

import { formatDate, formatDocumentTypeLabel, formatSize } from './mappers';

type DocumentsInspectorPanelProps = {
  canEdit: boolean;
  editDisabledReason?: string | null;
  locale: string;
  t: TFunction;
  inspectorFacts: number | null;
  inspectorSegments: number | null;
  lifecycle: DocumentLifecycle | null;
  readinessConfig: Record<DocumentReadiness, { label: string; cls: string }>;
  selectedDoc: DocumentItem;
  selectionMode: boolean;
  setAddLinkOpen: (open: boolean) => void;
  setCrawlMode: (value: string) => void;
  setDeleteDocOpen: (open: boolean) => void;
  setMaxDepth: (value: string) => void;
  setMaxPages: (value: string) => void;
  setReplaceFileOpen: (open: boolean) => void;
  setSeedUrl: (value: string) => void;
  updateSearchParamState: (updates: Record<string, string | null>) => void;
  onEdit: () => void;
  onRetry: () => void;
};

export function DocumentsInspectorPanel({
  canEdit,
  editDisabledReason,
  locale,
  t,
  inspectorFacts,
  inspectorSegments,
  lifecycle,
  readinessConfig,
  selectedDoc,
  selectionMode,
  setAddLinkOpen,
  setCrawlMode,
  setDeleteDocOpen,
  setMaxDepth,
  setMaxPages,
  setReplaceFileOpen,
  setSeedUrl,
  updateSearchParamState,
  onEdit,
  onRetry,
}: DocumentsInspectorPanelProps) {
  const typeLabel = formatDocumentTypeLabel(selectedDoc.fileType, selectedDoc.sourceKind, t);

  const openSource = () => {
    const href = selectedDoc.sourceAccess?.href ?? selectedDoc.sourceUri;
    if (!href) {
      return;
    }

    window.open(href, '_blank', 'noopener,noreferrer');
  };

  return (
    <div
      className={`inspector-panel w-80 lg:w-96 shrink-0 hidden md:block overflow-y-auto animate-slide-in-right ${
        selectionMode ? 'opacity-40 pointer-events-none' : ''
      }`}
    >
      <div className="p-4 border-b flex items-center justify-between">
        <h3 className="text-sm font-bold truncate tracking-tight">{selectedDoc.fileName}</h3>
        <button
          onClick={() => updateSearchParamState({ documentId: null })}
          className="p-1.5 rounded-lg hover:bg-muted transition-colors"
          aria-label={t('common.close')}
        >
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="p-4 space-y-5">
        <div>
          <span className={`status-badge ${readinessConfig[selectedDoc.readiness].cls}`}>
            {readinessConfig[selectedDoc.readiness].label}
          </span>
          {selectedDoc.stage && selectedDoc.readiness === 'processing' && (
            <span className="text-xs text-muted-foreground ml-2">{selectedDoc.stage}</span>
          )}
        </div>

        {selectedDoc.failureMessage && (
          <div className="inline-error">
            <div className="flex items-center gap-1.5 font-bold text-destructive mb-1.5">
              <XCircle className="h-3.5 w-3.5" /> {t('documents.error')}
            </div>
            {selectedDoc.failureMessage}
          </div>
        )}

        {selectedDoc.progressPercent != null && (
          <div>
            <div className="flex justify-between text-xs mb-2">
              <span className="font-semibold">{t('documents.progress')}</span>
              <span className="tabular-nums font-medium">{selectedDoc.progressPercent}%</span>
            </div>
            <div
              className="h-2 bg-surface-sunken rounded-full overflow-hidden"
              style={{ boxShadow: 'inset 0 1px 2px hsl(var(--foreground) / 0.04)' }}
            >
              <div
                className="h-full bg-primary rounded-full transition-all duration-500"
                style={{
                  width: `${selectedDoc.progressPercent}%`,
                  boxShadow: '0 0 8px -2px hsl(var(--primary) / 0.4)',
                }}
              />
            </div>
          </div>
        )}

        {/* Source link is available via the download button in actions */}

        <div className="space-y-2.5">
          <div className="section-label">
            {selectedDoc.sourceKind === 'web_page' ? t('documents.webSource') : t('documents.fileInfo')}
          </div>
          {[
            [t('documents.type'), typeLabel],
            [t('documents.size'), formatSize(selectedDoc.fileSize)],
            [t('documents.uploaded'), formatDate(selectedDoc.uploadedAt, locale)],
            [t('documents.documentId'), selectedDoc.id],
          ].map(([label, value]) => (
            <div key={label} className="flex justify-between text-sm">
              <span className="text-muted-foreground">{label}</span>
              <span className="font-mono text-xs font-semibold">{value}</span>
            </div>
          ))}
        </div>

        {lifecycle?.attempts?.[0]?.stageEvents?.length != null && lifecycle.attempts[0].stageEvents.length > 0 && (
          <div className="space-y-2">
            <div className="section-label">{t('documents.pipeline')}</div>
            <table className="w-full text-[11px] table-fixed">
              <colgroup>
                <col className="w-[35%]" />
                <col className="w-[15%]" />
                <col className="w-[30%]" />
                <col className="w-[20%]" />
              </colgroup>
              <thead>
                <tr className="text-left text-muted-foreground border-b">
                  <th className="pb-1 font-medium">{t('documents.pipelineStage')}</th>
                  <th className="pb-1 text-right font-medium">{t('documents.pipelineTime')}</th>
                  <th className="pb-1 text-right font-medium">{t('documents.pipelineModel')}</th>
                  <th className="pb-1 text-right font-medium">{t('documents.pipelineCost')}</th>
                </tr>
              </thead>
              <tbody>
                {lifecycle.attempts[0].stageEvents.map((se) => (
                  <tr key={se.stage} className="border-b border-border/30">
                    <td className="py-1 capitalize truncate">{se.stage.replace(/_/g, ' ')}</td>
                    <td className="py-1 text-right text-muted-foreground tabular-nums">
                      {se.elapsedMs != null ? `${(se.elapsedMs / 1000).toFixed(1)}s` : '\u2014'}
                    </td>
                    <td className="py-1 text-right text-muted-foreground truncate" title={se.modelName || undefined}>
                      {se.modelName ? se.modelName.replace('text-embedding-', 'embed-') : '\u2014'}
                    </td>
                    <td className="py-1 text-right text-muted-foreground tabular-nums">
                      {se.estimatedCost != null ? `$${Number(se.estimatedCost).toFixed(4)}` : '\u2014'}
                    </td>
                  </tr>
                ))}
                <tr className="font-semibold border-t">
                  <td className="py-1">{t('documents.pipelineTotal')}</td>
                  <td className="py-1 text-right tabular-nums">
                    {lifecycle.attempts[0].totalElapsedMs != null
                      ? `${(lifecycle.attempts[0].totalElapsedMs / 1000).toFixed(1)}s`
                      : '\u2014'}
                  </td>
                  <td />
                  <td className="py-1 text-right tabular-nums">
                    {lifecycle.totalCost != null
                      ? `$${Number(lifecycle.totalCost).toFixed(4)}`
                      : '\u2014'}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        )}

        <div className="space-y-2.5">
          <div className="section-label">{t('documents.preparation')}</div>
          <div className="text-xs text-muted-foreground">
            {selectedDoc.readiness === 'graph_ready' ||
            selectedDoc.readiness === 'readable' ||
            selectedDoc.readiness === 'graph_sparse' ? (
              <div className="space-y-1.5">
                <div className="flex justify-between">
                  <span>{t('documents.segments')}</span>
                  <span className="font-semibold text-foreground">{inspectorSegments ?? '...'}</span>
                </div>
                <div className="flex justify-between">
                  <span>{t('documents.technicalFacts')}</span>
                  <span className="font-semibold text-foreground">{inspectorFacts ?? '...'}</span>
                </div>
                <div className="flex justify-between">
                  <span>{t('documents.sourceFormat')}</span>
                  <span className="font-semibold text-foreground">
                    {typeLabel}
                  </span>
                </div>
              </div>
            ) : selectedDoc.readiness === 'processing' ? (
              <div className="flex items-center gap-2">
                <Loader2 className="h-3 w-3 animate-spin text-primary" /> {t('documents.processingEllipsis')}
              </div>
            ) : (
              <span>{t('documents.notYetAvailable')}</span>
            )}
          </div>
        </div>

        <div className="space-y-1.5">
          <div className="section-label">{t('documents.actions')}</div>
          <div
            className={`grid gap-2 ${
              (selectedDoc.sourceAccess?.href || selectedDoc.sourceUri) ? 'grid-cols-2' : 'grid-cols-1'
            }`}
          >
            <Button
              size="sm"
              className="w-full justify-start"
              onClick={onEdit}
              disabled={!canEdit}
              title={canEdit ? undefined : editDisabledReason ?? undefined}
            >
              <FilePenLine className="h-3.5 w-3.5 mr-2" /> {t('documents.edit')}
            </Button>
            {(selectedDoc.sourceAccess?.href || selectedDoc.sourceUri) && (
              <Button variant="outline" size="sm" className="w-full justify-start" onClick={openSource}>
                <Download className="h-3.5 w-3.5 mr-2" />
                {selectedDoc.sourceAccess?.kind === 'stored_document'
                  ? t('documents.downloadDocument')
                  : t('documents.openSourceUrl')}
              </Button>
            )}
          </div>
          {selectedDoc.canRetry && (
            <Button variant="outline" size="sm" className="w-full justify-start" onClick={onRetry}>
              <RotateCw className="h-3.5 w-3.5 mr-2" /> {t('documents.retryProcessing')}
            </Button>
          )}
          {selectedDoc.sourceKind === 'web_page' && selectedDoc.sourceUri && (
            <Button
              variant="outline"
              size="sm"
              className="w-full justify-start"
              onClick={() => {
                setSeedUrl(selectedDoc.sourceUri || '');
                setCrawlMode('single_page');
                setMaxDepth('1');
                setMaxPages('10');
                setAddLinkOpen(true);
              }}
            >
              <Globe className="h-3.5 w-3.5 mr-2" /> {t('documents.reIngest')}
            </Button>
          )}
          <Button variant="outline" size="sm" className="w-full justify-start" onClick={() => setReplaceFileOpen(true)}>
            <Upload className="h-3.5 w-3.5 mr-2" /> {t('documents.replaceFile')}
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="w-full justify-start text-destructive hover:text-destructive"
            onClick={() => setDeleteDocOpen(true)}
          >
            <Trash2 className="h-3.5 w-3.5 mr-2" /> {t('documents.delete')}
          </Button>
        </div>
      </div>
    </div>
  );
}
