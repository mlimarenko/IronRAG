import { memo } from 'react';
import type { TFunction } from 'i18next';
import { ArrowRight, Globe } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { RecentWebRun } from './types';
import { formatDateTime, hostnameFromUrl, runStateClass } from './format';

type LatestIngestPanelProps = {
  t: TFunction;
  locale: string;
  latestRun: RecentWebRun | undefined;
  onNavigate: (path: string) => void;
};

function LatestIngestPanelImpl({ t, locale, latestRun, onNavigate }: LatestIngestPanelProps) {
  const emptyLabel = t('dashboard.notAvailable');
  return (
    <div className="workbench-surface p-5 sm:p-6">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-bold tracking-tight">{t('dashboard.latestIngest')}</h2>
        {latestRun ? (
          <span className={`status-badge ${runStateClass(latestRun.runState)}`}>
            {t(`dashboard.runStateLabels.${latestRun.runState}`)}
          </span>
        ) : null}
      </div>

      {latestRun ? (
        <>
          <div className="mt-4">
            <div className="flex items-center gap-2 text-sm font-semibold text-foreground">
              <Globe className="h-4 w-4 text-muted-foreground" />
              <span className="truncate">{hostnameFromUrl(latestRun.seedUrl)}</span>
            </div>
            <div className="mt-1 truncate text-xs text-muted-foreground">{latestRun.seedUrl}</div>
          </div>

          <div className="mt-4 grid grid-cols-3 gap-3">
            {[
              {
                label: t('dashboard.processed'),
                value: latestRun.counts.processed,
                className: 'text-status-ready',
              },
              {
                label: t('dashboard.queued'),
                value: latestRun.counts.queued + latestRun.counts.processing,
                className: 'text-status-processing',
              },
              {
                label: t('dashboard.failed'),
                value: latestRun.counts.failed + latestRun.counts.blocked,
                className: 'text-status-failed',
              },
            ].map((item) => (
              <div
                key={item.label}
                className="rounded-xl border border-border/60 bg-background/70 p-3"
              >
                <div className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                  {item.label}
                </div>
                <div
                  className={`mt-2 text-xl font-bold tracking-tight tabular-nums ${item.className}`}
                >
                  {item.value}
                </div>
              </div>
            ))}
          </div>

          <div className="mt-4 flex items-center justify-between gap-3 text-xs text-muted-foreground">
            <span>{t('dashboard.lastActivity')}</span>
            <span className="text-right">
              {formatDateTime(latestRun.lastActivityAt, locale, emptyLabel)}
            </span>
          </div>

          <Button
            variant="outline"
            size="sm"
            className="mt-4 w-full justify-between"
            onClick={() => onNavigate('/documents')}
          >
            {t('dashboard.openDocuments')}
            <ArrowRight className="h-3.5 w-3.5" />
          </Button>
        </>
      ) : (
        <div className="mt-4 rounded-xl border border-dashed border-border/70 bg-background/60 p-4 text-sm text-muted-foreground">
          {t('dashboard.noRecentRuns')}
        </div>
      )}
    </div>
  );
}

export const LatestIngestPanel = memo(LatestIngestPanelImpl);
