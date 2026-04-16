import { useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import {
  FilePenLine,
  Download,
  ExternalLink,
  Loader2,
  RotateCw,
  Trash2,
  Upload,
  X,
  XCircle,
} from 'lucide-react';

import { Button } from '@/components/ui/button';
import type { DocumentItem, DocumentLifecycle } from '@/types';
import { compactText, truncatedTitle } from '@/lib/compactText';

import {
  buildDocumentStatusBadgeConfig,
  formatDate,
  formatDocumentTypeLabel,
  formatSize,
  isWebPageDocument,
} from '@/adapters/documents';

type DocumentsInspectorPanelProps = {
  canEdit: boolean;
  editDisabledReason?: string | null;
  locale: string;
  t: TFunction;
  inspectorFacts: number | null;
  inspectorSegments: number | null;
  lifecycle: DocumentLifecycle | null;
  selectedDoc: DocumentItem;
  selectionMode: boolean;
  setDeleteDocOpen: (open: boolean) => void;
  setReplaceFileOpen: (open: boolean) => void;
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
  selectedDoc,
  selectionMode,
  setDeleteDocOpen,
  setReplaceFileOpen,
  updateSearchParamState,
  onEdit,
  onRetry,
}: DocumentsInspectorPanelProps) {
  const isWebPage = isWebPageDocument(
    selectedDoc.sourceKind,
    selectedDoc.sourceUri,
    selectedDoc.fileName,
  );
  const displayName =
    isWebPage && selectedDoc.sourceUri ? selectedDoc.sourceUri : selectedDoc.fileName;
  const [showFullName, setShowFullName] = useState(false);
  const compactDisplayName = compactText(displayName, 96);
  const typeLabel = formatDocumentTypeLabel(selectedDoc.fileType, selectedDoc.sourceKind, t, {
    sourceUri: selectedDoc.sourceUri,
    fileName: selectedDoc.fileName,
  });
  const compactTypeLabel = compactText(typeLabel, 54);
  const statusBadge = buildDocumentStatusBadgeConfig(t)[selectedDoc.status];

  useEffect(() => {
    setShowFullName(false);
  }, [selectedDoc.id]);

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
      <div className="p-4 border-b flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h3
            className="text-sm font-bold tracking-tight leading-5 [overflow-wrap:anywhere]"
            title={compactDisplayName.isTruncated && !showFullName ? displayName : undefined}
          >
            {showFullName || !compactDisplayName.isTruncated
              ? displayName
              : compactDisplayName.text}
          </h3>
          {compactDisplayName.isTruncated && (
            <button
              type="button"
              className="mt-1 text-xs font-medium text-primary hover:text-primary/80"
              onClick={() => setShowFullName((value) => !value)}
            >
              {showFullName ? t('documents.showLessName') : t('documents.showFullName')}
            </button>
          )}
        </div>
        <button
          onClick={() => updateSearchParamState({ documentId: null })}
          className="shrink-0 p-1.5 rounded-lg hover:bg-muted transition-colors"
          aria-label={t('common.close')}
        >
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="p-4 space-y-5">
        <div>
          <span className={`status-badge ${statusBadge.cls}`} title={selectedDoc.statusReason}>
            {statusBadge.label}
          </span>
          {selectedDoc.stage &&
            (selectedDoc.status === 'processing' || selectedDoc.status === 'queued') && (
              <span className="text-xs text-muted-foreground ml-2">{selectedDoc.stage}</span>
            )}
        </div>

        {selectedDoc.statusReason && selectedDoc.status === 'failed' && (
          <div className="inline-error">
            <div className="flex items-center gap-1.5 font-bold text-destructive mb-1.5">
              <XCircle className="h-3.5 w-3.5" />
              {t('documents.attention')}
            </div>
            {selectedDoc.statusReason}
          </div>
        )}

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
            {isWebPage ? t('documents.webSource') : t('documents.fileInfo')}
          </div>
          {[
            [t('documents.type'), compactTypeLabel.text],
            [t('documents.size'), formatSize(selectedDoc.fileSize)],
            [t('documents.uploaded'), formatDate(selectedDoc.uploadedAt, locale)],
            [t('documents.documentId'), selectedDoc.id],
          ].map(([label, value]) => (
            <div key={label} className="flex justify-between gap-3 text-sm">
              <span className="text-muted-foreground">{label}</span>
              <span
                className="font-mono text-xs font-semibold text-right [overflow-wrap:anywhere]"
                title={label === t('documents.type') ? truncatedTitle(compactTypeLabel) : undefined}
              >
                {value}
              </span>
            </div>
          ))}
        </div>

        {lifecycle?.attempts?.[0]?.stageEvents?.length != null && lifecycle.attempts[0].stageEvents.length > 0 && (
          <div className="space-y-2">
            <div className="section-label">{t('documents.pipeline')}</div>
            {/* Column widths tuned for the canonical ~360 px inspector
                panel. Model names like `qwen3-embedding:0.6b` or
                `text-embedding-3-large` are the longest cell content,
                so Model gets the biggest share and is allowed to wrap
                via `break-words` instead of clipping with `truncate`.
                The title attribute still shows the full name on hover
                for the rare super-long variant. */}
            <table className="w-full text-[11px] table-fixed">
              <colgroup>
                <col className="w-[28%]" />
                <col className="w-[14%]" />
                <col className="w-[42%]" />
                <col className="w-[16%]" />
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
                    <td className="py-1 capitalize break-words">{se.stage.replace(/_/g, ' ')}</td>
                    <td className="py-1 text-right text-muted-foreground tabular-nums whitespace-nowrap">
                      {se.elapsedMs != null ? `${(se.elapsedMs / 1000).toFixed(1)}s` : '\u2014'}
                    </td>
                    <td
                      className="py-1 text-right text-muted-foreground font-mono text-[10px] [overflow-wrap:anywhere] leading-tight"
                      title={se.modelName || undefined}
                    >
                      {se.modelName ? se.modelName.replace('text-embedding-', 'embed-') : '\u2014'}
                    </td>
                    <td className="py-1 text-right text-muted-foreground tabular-nums whitespace-nowrap">
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
                  <span className="font-semibold text-foreground" title={truncatedTitle(compactTypeLabel)}>
                    {compactTypeLabel.text}
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
          <Button variant="outline" size="sm" className="w-full justify-start" onClick={onRetry}>
            <RotateCw className="h-3.5 w-3.5 mr-2" /> {t('documents.retryProcessing')}
          </Button>
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
