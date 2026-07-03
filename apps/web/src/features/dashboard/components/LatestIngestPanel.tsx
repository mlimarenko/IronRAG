import { memo } from 'react';
import type { TFunction } from 'i18next';
import { Globe } from 'lucide-react';
import { StatusBadge } from '@/shared/components/StatusBadge';
import type { RecentWebRun } from "../model/types";
import { formatDateTime, hostnameFromUrl, runStateClass, toStatusTone } from "../model/format";

type LatestIngestPanelProps = {
  t: TFunction;
  locale: string;
  latestRun: RecentWebRun | undefined;
};

function LatestIngestPanelImpl({ t, locale, latestRun }: LatestIngestPanelProps) {
  const emptyLabel = t('dashboard.notAvailable');
  return (
    <div className="workbench-surface h-full p-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-bold tracking-tight">{t('dashboard.latestIngest')}</h2>
        {latestRun ? (
          <StatusBadge tone={toStatusTone(runStateClass(latestRun.runState))}>
            {t(`dashboard.runStateLabels.${latestRun.runState}`)}
          </StatusBadge>
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

          <div className="mt-4 grid grid-cols-3 gap-2 border-t pt-3">
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
                className="rounded-md bg-surface-sunken p-2.5"
              >
                <div className="section-label">
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
        </>
      ) : (
        <div className="mt-4 rounded-lg border border-dashed border-border bg-surface-sunken/40 p-4 text-sm text-muted-foreground">
          {t('dashboard.noRecentRuns')}
        </div>
      )}
    </div>
  );
}

export const LatestIngestPanel = memo(LatestIngestPanelImpl);
