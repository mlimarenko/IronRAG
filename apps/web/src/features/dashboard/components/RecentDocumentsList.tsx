import { memo } from 'react'
import type { TFunction } from 'i18next'
import { FileText } from 'lucide-react'
import { StatusBadge } from '@/shared/components/StatusBadge'
import { humanizeDocumentFailure, humanizeDocumentStage } from '@/shared/lib/document-processing'
import type { RecentDocument } from '../model/types'
import { buildDocumentsPath } from '../model/types'
import { formatRelativeTime, formatSize, readinessClass, toStatusTone } from '../model/format'

type RecentDocumentsListProps = Readonly<{
  t: TFunction
  locale: string
  recentDocuments: RecentDocument[]
  totalDocuments: number
  onNavigate: (path: string) => void
}>

function buildDetailBits(doc: RecentDocument, t: TFunction): string[] {
  const bits: string[] = []

  if (doc.readiness === 'failed' && doc.failureMessage) {
    bits.push(
      humanizeDocumentFailure(
        {
          stalledReason: doc.failureMessage,
          stage: doc.stageLabel ?? null,
        },
        t,
      ) ?? doc.failureMessage,
    )
  } else if (doc.readiness === 'processing' && doc.stageLabel) {
    bits.push(humanizeDocumentStage(doc.stageLabel, t) ?? doc.stageLabel)
  } else {
    if ((doc.preparedSegmentCount ?? 0) > 0) {
      bits.push(t('dashboard.segmentsSummary', { count: doc.preparedSegmentCount ?? 0 }))
    }
    if ((doc.technicalFactCount ?? 0) > 0) {
      bits.push(t('dashboard.factsSummary', { count: doc.technicalFactCount ?? 0 }))
    }
  }

  if (doc.canRetry) bits.push(t('dashboard.retryAvailable'))

  return bits
}

function RecentDocumentsListImpl({
  t,
  locale,
  recentDocuments,
  totalDocuments,
  onNavigate,
}: RecentDocumentsListProps) {
  return (
    <div className="workbench-surface h-full p-4">
      <div>
        <h2 className="text-sm font-bold tracking-tight">{t('dashboard.recentDocs')}</h2>
        <p className="mt-1 text-xs text-muted-foreground">
          {t('dashboard.recentDocsSummary', {
            count: recentDocuments.length,
            total: totalDocuments,
          })}
        </p>
      </div>

      {recentDocuments.length > 0 ? (
        <div className="mt-4 grid gap-3 xl:grid-cols-2">
          {recentDocuments.map((doc) => {
            const detailBits = buildDetailBits(doc, t)

            return (
              <button
                key={doc.id}
                type="button"
                onClick={() => onNavigate(buildDocumentsPath({ documentId: doc.id }))}
                className="w-full rounded-lg bg-surface-sunken p-3 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
              >
                <div className="flex items-start gap-3">
                  <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted">
                    <FileText className="h-4 w-4 text-muted-foreground" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="truncate text-sm font-semibold text-foreground">
                          {doc.fileName}
                        </div>
                        <div className="mt-1 text-2xs text-muted-foreground">
                          {formatRelativeTime(doc.uploadedAt, locale)}
                          <span className="mx-1 text-border">·</span>
                          {formatSize(doc.fileSize)}
                        </div>
                      </div>
                      <StatusBadge
                        tone={toStatusTone(readinessClass(doc.readiness))}
                        className="shrink-0"
                      >
                        {t(`dashboard.readinessLabels.${doc.readiness}`)}
                      </StatusBadge>
                    </div>

                    {detailBits.length > 0 ? (
                      <div
                        className={`mt-2 text-2xs leading-relaxed ${
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
            )
          })}
        </div>
      ) : (
        <div className="mt-4 rounded-lg border border-dashed border-border bg-surface-sunken/40 p-4 text-sm text-muted-foreground">
          {t('dashboard.noDocs')}
        </div>
      )}
    </div>
  )
}

export const RecentDocumentsList = memo(RecentDocumentsListImpl)
