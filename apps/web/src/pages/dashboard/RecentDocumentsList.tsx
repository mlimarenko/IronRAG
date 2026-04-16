import { memo } from 'react';
import type { TFunction } from 'i18next';
import { FileText } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { humanizeDocumentFailure, humanizeDocumentStage } from '@/lib/document-processing';
import type { RecentDocument } from './types';
import { buildDocumentsPath } from './types';
import { formatRelativeTime, formatSize, readinessClass } from './format';

type RecentDocumentsListProps = {
  t: TFunction;
  locale: string;
  recentDocuments: RecentDocument[];
  totalDocuments: number;
  onNavigate: (path: string) => void;
};

function buildDetailBits(doc: RecentDocument, t: TFunction): string[] {
  const bits: string[] = [];

  if (doc.readiness === 'failed' && doc.failureMessage) {
    bits.push(
      humanizeDocumentFailure(
        {
          failureCode: doc.failureMessage,
          stalledReason: doc.failureMessage,
          stage: doc.stageLabel,
        },
        t,
      ) ?? doc.failureMessage,
    );
  } else if (doc.readiness === 'processing' && doc.stageLabel) {
    bits.push(humanizeDocumentStage(doc.stageLabel, t) ?? doc.stageLabel);
  } else {
    if ((doc.preparedSegmentCount ?? 0) > 0) {
      bits.push(t('dashboard.segmentsSummary', { count: doc.preparedSegmentCount ?? 0 }));
    }
    if ((doc.technicalFactCount ?? 0) > 0) {
      bits.push(t('dashboard.factsSummary', { count: doc.technicalFactCount ?? 0 }));
    }
  }

  if (doc.canRetry) bits.push(t('dashboard.retryAvailable'));

  return bits;
}

function RecentDocumentsListImpl({
  t,
  locale,
  recentDocuments,
  totalDocuments,
  onNavigate,
}: RecentDocumentsListProps) {
  return (
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
        <Button variant="outline" size="sm" onClick={() => onNavigate('/documents')}>
          <FileText className="h-3.5 w-3.5 mr-1.5" />
          {t('dashboard.openDocuments')}
        </Button>
      </div>

      {recentDocuments.length > 0 ? (
        <div className="mt-4 grid gap-3 xl:grid-cols-2">
          {recentDocuments.map((doc) => {
            const detailBits = buildDetailBits(doc, t);

            return (
              <button
                key={doc.id}
                type="button"
                onClick={() => onNavigate(buildDocumentsPath({ documentId: doc.id }))}
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
                          {formatRelativeTime(doc.uploadedAt, locale)}
                          <span className="mx-1 text-border">·</span>
                          {formatSize(doc.fileSize)}
                        </div>
                      </div>
                      <span
                        className={`status-badge shrink-0 text-[10px] ${readinessClass(doc.readiness)}`}
                      >
                        {t(`dashboard.readinessLabels.${doc.readiness}`)}
                      </span>
                    </div>

                    {detailBits.length > 0 ? (
                      <div
                        className={`mt-2 text-[11px] leading-relaxed ${
                          doc.readiness === 'failed'
                            ? 'text-status-failed'
                            : 'text-muted-foreground'
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
  );
}

export const RecentDocumentsList = memo(RecentDocumentsListImpl);
