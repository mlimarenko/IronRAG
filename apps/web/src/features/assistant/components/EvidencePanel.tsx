import { memo } from 'react'
import type { TFunction } from 'i18next'
import { FileText, Share2, X } from 'lucide-react'
import { Button } from '@/shared/components/ui/button'
import { cn } from '@/shared/lib/utils'
import type { EvidenceBundle } from '@/shared/types'
import { shouldShowVerifiedEvidence } from '../model/verification'
import { VerificationChip } from './VerificationChip'

type EvidencePanelProps = Readonly<{
  t: TFunction
  evidence: EvidenceBundle
  /** Optional close handler; when present, a close control is shown. */
  onClose?: () => void
  /** Extra classes for layout (width / responsive visibility) at the call site. */
  className?: string
  onOpenDocuments: () => void
  onOpenGraph: () => void
}>

function formatRelevance(value: number): string {
  // The relevance field is a mixed bag: entity/relation references carry a
  // normalized probability in [0, 1], chunk/segment search hits carry a
  // raw BM25 (or boosted BM25) score that can reach double- or
  // triple-digits. Previously we multiplied everything by 100 unless
  // value > 100, which produced "6384%" for a BM25 = 63.84 hit. Now we
  // distinguish: anything within [0, 1] gets a percentage, anything
  // above is a raw score shown with two decimals.
  if (!Number.isFinite(value)) return '—'
  if (value <= 1) {
    return `${(Math.max(0, value) * 100).toFixed(0)}%`
  }
  return value.toFixed(2)
}

function EvidencePanelImpl({
  t,
  evidence,
  onClose,
  className,
  onOpenDocuments,
  onOpenGraph,
}: EvidencePanelProps) {
  const showVerdict = shouldShowVerifiedEvidence(evidence)

  return (
    <div className={cn('flex h-full min-h-0 shrink-0 flex-col overflow-hidden', className)}>
      <div className="flex shrink-0 items-center justify-between gap-2 border-b p-3">
        <h3 className="text-sm font-bold tracking-tight">{t('assistant.evidence')}</h3>
        {onClose && (
          <Button
            type="button"
            size="icon"
            variant="ghost"
            className="h-7 w-7 shrink-0"
            aria-label={t('assistant.close')}
            title={t('assistant.close')}
            onClick={onClose}
          >
            <X className="h-4 w-4" />
          </Button>
        )}
      </div>
      <div
        className="min-h-0 flex-1 space-y-4 overflow-y-auto overscroll-contain p-3"
        data-testid="assistant-evidence-scroll"
      >
        {showVerdict && (
          <VerificationChip
            t={t}
            state={evidence.verificationState}
            warnings={evidence.verificationWarnings}
          />
        )}

        {evidence.runtimeSummary && (
          <div>
            <div className="section-label mb-2">{t('assistant.runtime')}</div>
            <div className="grid grid-cols-2 gap-2 text-xs">
              {[
                { label: t('assistant.segmentRefs'), value: evidence.runtimeSummary.totalSegments },
                { label: t('assistant.factRefs'), value: evidence.runtimeSummary.totalFacts },
                { label: t('assistant.entityRefs'), value: evidence.runtimeSummary.totalEntities },
                {
                  label: t('assistant.relationRefs'),
                  value: evidence.runtimeSummary.totalRelations,
                },
              ].map((m) => (
                <div key={m.label} className="p-3 bg-surface-sunken rounded-xl">
                  <div className="section-label font-bold">{m.label}</div>
                  <div className="font-bold text-base mt-1 tabular-nums">{m.value}</div>
                </div>
              ))}
            </div>
          </div>
        )}

        {evidence.segmentRefs.length > 0 && (
          <div>
            <div className="section-label mb-2">{t('assistant.segmentRefs')}</div>
            <div className="space-y-2">
              {evidence.segmentRefs.map((ref) => (
                <div
                  key={`${ref.documentId}-${ref.segmentOrdinal}`}
                  className="p-3.5 workbench-surface text-xs min-w-0"
                >
                  <div className="flex items-start gap-1.5 font-bold min-w-0">
                    <FileText className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                    <span
                      className="min-w-0 flex-1 break-words"
                      title={ref.documentTitle || ref.documentName}
                    >
                      {ref.documentTitle || ref.documentName}
                    </span>
                  </div>
                  {(ref.sourceAccess?.href || ref.sourceUri) && (
                    <a
                      href={ref.sourceAccess?.href ?? ref.sourceUri ?? '#'}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-primary text-2xs hover:underline truncate block mt-0.5"
                      title={ref.sourceUri ?? undefined}
                    >
                      {ref.sourceAccess?.kind === 'stored_document'
                        ? t('assistant.openSourceDocument')
                        : (ref.sourceUri ?? t('assistant.openSourceLink'))}
                    </a>
                  )}
                  <p className="mt-1.5 text-muted-foreground line-clamp-2 leading-relaxed">
                    {ref.excerpt}
                  </p>
                  <div className="mt-1.5 text-muted-foreground">
                    {t('assistant.relevance')}:{' '}
                    <span className="font-bold text-foreground">
                      {formatRelevance(ref.relevance)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {evidence.factRefs.length > 0 && (
          <div>
            <div className="section-label mb-2">{t('assistant.factRefs')}</div>
            <div className="space-y-2">
              {evidence.factRefs.map((ref) => (
                <div
                  key={`${ref.factKind}-${ref.value}`}
                  className="p-3.5 workbench-surface text-xs"
                >
                  <div className="font-bold">{ref.value}</div>
                  <div className="text-muted-foreground mt-1">
                    {ref.factKind}
                    {ref.confidence > 0 ? ` · ${formatRelevance(ref.confidence)}` : ''}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {evidence.entityRefs.length > 0 && (
          <div>
            <div className="section-label mb-2">{t('assistant.entityRefs')}</div>
            <div className="space-y-1">
              {evidence.entityRefs.map((ref) => (
                <button
                  key={`${ref.entityId}-${ref.label}`}
                  className="w-full flex items-center gap-2.5 p-3 workbench-surface text-xs text-left hover:bg-accent/50 transition-all duration-200"
                  onClick={onOpenGraph}
                >
                  <Share2 className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="font-bold">{ref.label}</span>
                  <span className="text-muted-foreground ml-auto">{ref.type}</span>
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="space-y-1.5 pt-2">
          <Button
            variant="outline"
            size="sm"
            className="w-full justify-start"
            onClick={onOpenDocuments}
          >
            <FileText className="h-3.5 w-3.5 mr-2" /> {t('assistant.openDocuments')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="w-full justify-start"
            onClick={onOpenGraph}
          >
            <Share2 className="h-3.5 w-3.5 mr-2" /> {t('assistant.openGraph')}
          </Button>
        </div>
      </div>
    </div>
  )
}

export const EvidencePanel = memo(EvidencePanelImpl)
