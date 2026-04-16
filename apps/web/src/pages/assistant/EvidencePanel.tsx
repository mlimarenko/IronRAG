import { memo } from 'react';
import type { TFunction } from 'i18next';
import { FileText, Share2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { EvidenceBundle } from '@/types';
import { VERIFICATION_CONFIG } from './verificationConfig';

type EvidencePanelProps = {
  t: TFunction;
  evidence: EvidenceBundle;
  onOpenDocuments: () => void;
  onOpenGraph: () => void;
};

function formatRelevance(value: number): string {
  if (value > 100) return Math.round(value).toLocaleString();
  return `${(value * 100).toFixed(0)}%`;
}

function EvidencePanelImpl({ t, evidence, onOpenDocuments, onOpenGraph }: EvidencePanelProps) {
  const vc =
    evidence.verificationState !== 'not_run'
      ? VERIFICATION_CONFIG[evidence.verificationState]
      : null;

  return (
    <div className="inspector-panel w-72 lg:w-80 shrink-0 hidden lg:block overflow-y-auto animate-slide-in-right">
      <div className="p-3 border-b">
        <h3 className="text-sm font-bold tracking-tight">{t('assistant.evidence')}</h3>
      </div>
      <div className="p-3 space-y-4">
        {vc && (
          <div
            className="flex items-center gap-2.5 p-3.5 rounded-xl"
            style={{
              background:
                evidence.verificationState === 'passed'
                  ? 'hsl(var(--status-ready-bg))'
                  : 'hsl(var(--status-warning-bg))',
              boxShadow: `inset 0 0 0 1px ${
                evidence.verificationState === 'passed'
                  ? 'hsl(var(--status-ready-ring) / 0.3)'
                  : 'hsl(var(--status-warning-ring) / 0.3)'
              }`,
            }}
          >
            <vc.icon className={`h-4 w-4 ${vc.cls}`} />
            <span className="text-sm font-bold">{t(vc.labelKey)}</span>
          </div>
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
                  <div className="text-muted-foreground text-[10px] font-bold uppercase tracking-wider">
                    {m.label}
                  </div>
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
              {evidence.segmentRefs.map((ref, i) => (
                <div
                  key={i}
                  className="p-3.5 border rounded-xl text-xs bg-card shadow-soft min-w-0"
                >
                  <div className="flex items-start gap-1.5 font-bold min-w-0">
                    <FileText className="h-3 w-3 mt-0.5 shrink-0" />
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
                      className="text-primary text-[10px] hover:underline truncate block mt-0.5"
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
              {evidence.factRefs.map((ref, i) => (
                <div key={i} className="p-3.5 border rounded-xl text-xs bg-card shadow-soft">
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
              {evidence.entityRefs.map((ref, i) => (
                <button
                  key={i}
                  className="w-full flex items-center gap-2.5 p-3 border rounded-xl text-xs text-left hover:bg-accent/50 transition-all duration-200 bg-card shadow-soft"
                  onClick={onOpenGraph}
                >
                  <Share2 className="h-3 w-3 text-muted-foreground" />
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
  );
}

export const EvidencePanel = memo(EvidencePanelImpl);
