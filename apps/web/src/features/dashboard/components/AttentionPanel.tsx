import { memo } from 'react';
import type { TFunction } from 'i18next';
import { AlertTriangle, ArrowRight, CheckCircle2, Clock, XCircle } from 'lucide-react';
import { StatusBadge } from '@/shared/components/StatusBadge';
import type { DashboardAttentionItem } from "../model/types";
import { attentionClass, localizeAttention, resolveAttentionRoute } from "../model/format";

type AttentionPanelProps = {
  t: TFunction;
  attention: DashboardAttentionItem[];
  onNavigate: (path: string) => void;
};

function AttentionPanelImpl({
  t,
  attention,
  onNavigate,
}: AttentionPanelProps) {
  return (
    <div className="workbench-surface p-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-bold tracking-tight">{t('dashboard.attentionRequired')}</h2>
        <StatusBadge tone={attention.length > 0 ? 'failed' : 'ready'}>
          {attention.length}
        </StatusBadge>
      </div>

      {attention.length > 0 ? (
        <div className="mt-4 space-y-2">
          {attention.map((item) => {
            const content = localizeAttention(item, t);
            const route = resolveAttentionRoute(item);

            return (
              <button
                key={item.code}
                type="button"
                onClick={() => onNavigate(route)}
                className="w-full rounded-lg bg-surface-sunken p-3 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
              >
                <div className="flex items-start gap-3">
                  <div
                    className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md ${attentionClass(item.level)}`}
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
                    </div>
                    <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                      {content.detail}
                    </p>
                    <div className="mt-2 inline-flex items-center gap-1.5 text-2xs font-semibold text-primary">
                      <span>{content.action}</span>
                      <ArrowRight className="h-3.5 w-3.5" />
                    </div>
                  </div>
                </div>
              </button>
            );
          })}
        </div>
      ) : (
        <div className="mt-4 rounded-lg bg-surface-sunken p-3">
          <div className="flex items-center gap-3">
            <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md status-ready">
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
