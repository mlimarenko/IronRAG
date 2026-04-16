import { memo } from 'react';
import type { TFunction } from 'i18next';
import { AlertTriangle, ArrowRight, CheckCircle2, Clock, XCircle } from 'lucide-react';
import type { DashboardAttentionItem } from './types';
import { attentionClass, localizeAttention, resolveAttentionRoute } from './format';

type AttentionPanelProps = {
  t: TFunction;
  attention: DashboardAttentionItem[];
  graphCoverageActionPath: string;
  onNavigate: (path: string) => void;
};

function AttentionPanelImpl({
  t,
  attention,
  graphCoverageActionPath,
  onNavigate,
}: AttentionPanelProps) {
  return (
    <div className="workbench-surface p-5 sm:p-6">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-bold tracking-tight">{t('dashboard.attentionRequired')}</h2>
        <span
          className={`status-badge ${attention.length > 0 ? 'status-failed' : 'status-ready'}`}
        >
          {attention.length}
        </span>
      </div>

      {attention.length > 0 ? (
        <div className="mt-4 space-y-2">
          {attention.map((item) => {
            const content = localizeAttention(item, t);
            const route = resolveAttentionRoute(item, graphCoverageActionPath);

            return (
              <button
                key={item.code}
                type="button"
                onClick={() => onNavigate(route)}
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
              <div className="text-sm font-semibold text-foreground">
                {t('dashboard.allHealthy')}
              </div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t('dashboard.noAttentionDesc')}
              </p>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export const AttentionPanel = memo(AttentionPanelImpl);
