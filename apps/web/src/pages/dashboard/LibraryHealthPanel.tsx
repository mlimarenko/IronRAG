import { memo } from 'react';
import type { TFunction } from 'i18next';
import { Activity, Database, Share2 } from 'lucide-react';
import type { DashboardGraph } from './types';
import { formatDateTime, graphStatusClass } from './format';

export type HealthRow = {
  key: string;
  label: string;
  count: number;
  className: string;
  actionPath: string;
};

type LibraryHealthPanelProps = {
  t: TFunction;
  locale: string;
  graph: DashboardGraph;
  totalDocuments: number;
  readyCount: number;
  graphReadyCount: number;
  readableWithoutGraphCount: number;
  healthRows: HealthRow[];
  onNavigate: (path: string) => void;
};

function LibraryHealthPanelImpl({
  t,
  locale,
  graph,
  totalDocuments,
  readyCount,
  graphReadyCount,
  readableWithoutGraphCount,
  healthRows,
  onNavigate,
}: LibraryHealthPanelProps) {
  const emptyLabel = t('dashboard.notAvailable');
  return (
    <div className="workbench-surface p-5 sm:p-6">
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <h2 className="text-sm font-bold tracking-tight">{t('dashboard.libraryHealth')}</h2>
          <p className="text-xs text-muted-foreground mt-1.5">
            {totalDocuments > 0
              ? t('dashboard.graphCoverageSummary', {
                  ready: graphReadyCount,
                  total: totalDocuments,
                })
              : t('dashboard.noDocs')}
            {totalDocuments > 0 && (
              <>
                <span className="mx-1.5 text-border">·</span>
                {t('dashboard.documentsReadySummary', { count: readyCount })}
              </>
            )}
            {readableWithoutGraphCount > 0 && (
              <>
                <span className="mx-1.5 text-border">·</span>
                {t('dashboard.readableNoGraphSummary', { count: readableWithoutGraphCount })}
              </>
            )}
          </p>
        </div>
        <div className="flex flex-col items-start gap-2 sm:items-end">
          <span className={`status-badge ${graphStatusClass(graph.status)}`}>
            {t(`dashboard.graphStatusLabels.${graph.status}`)}
          </span>
          <span className="text-xs text-muted-foreground">
            {t('dashboard.updated')}: {formatDateTime(graph.updatedAt, locale, emptyLabel)}
          </span>
        </div>
      </div>

      <div className="mt-6 space-y-4">
        {healthRows.map((row) => {
          const ratio =
            totalDocuments > 0
              ? Math.min(100, Math.round((row.count / totalDocuments) * 100))
              : 0;

          return (
            <button
              key={row.key}
              type="button"
              onClick={() => onNavigate(row.actionPath)}
              className="block w-full rounded-lg px-0.5 py-1 text-left transition-colors hover:bg-accent/25 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
            >
              <div className="flex items-center justify-between gap-3 text-xs">
                <span className="font-semibold text-foreground">{row.label}</span>
                <span className="text-muted-foreground tabular-nums">
                  {row.count}
                  {totalDocuments > 0 && <span className="ml-1">{ratio}%</span>}
                </span>
              </div>
              <div className="mt-2 h-2 rounded-full bg-surface-sunken overflow-hidden">
                <div
                  className={`h-full rounded-full transition-all duration-700 ease-out ${row.className}`}
                  style={{ width: `${ratio}%` }}
                />
              </div>
            </button>
          );
        })}
      </div>

      <div className="mt-6 grid grid-cols-3 gap-3">
        {[
          { label: t('dashboard.nodes'), value: graph.nodeCount, icon: Share2 },
          { label: t('dashboard.edges'), value: graph.edgeCount, icon: Activity },
          {
            label: t('dashboard.factDocs'),
            value: graph.typedFactDocumentCount,
            icon: Database,
          },
        ].map((item) => (
          <div
            key={item.label}
            className="rounded-xl border border-border/60 bg-background/70 p-3.5"
          >
            <div className="flex items-center gap-2 text-muted-foreground">
              <item.icon className="h-3.5 w-3.5" />
              <span className="text-[11px] font-semibold uppercase tracking-wider">
                {item.label}
              </span>
            </div>
            <div className="mt-2 text-xl font-bold tracking-tight tabular-nums">
              {item.value}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export const LibraryHealthPanel = memo(LibraryHealthPanelImpl);
