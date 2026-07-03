import { memo } from 'react';
import type { TFunction } from 'i18next';
import { Activity, Database, Share2 } from 'lucide-react';
import { StatusBadge } from '@/shared/components/StatusBadge';
import type { DashboardGraph } from "../model/types";
import { formatDateTime, graphStatusClass, toStatusTone } from "../model/format";

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
  readableWithoutGraphCount,
  healthRows,
  onNavigate,
}: LibraryHealthPanelProps) {
  const emptyLabel = t('dashboard.notAvailable');
  const visibleHealthRows = healthRows.filter((row) => row.key === 'graph-ready' || row.count > 0);

  return (
    <div className="workbench-surface p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-bold tracking-tight">{t('dashboard.libraryHealth')}</h2>
          <p className="text-xs text-muted-foreground mt-1.5">
            {totalDocuments > 0
              ? t('dashboard.documentsReadySummary', { count: readyCount })
              : t('dashboard.noDocs')}
            {readableWithoutGraphCount > 0 && (
              <>
                <span className="mx-1.5 text-border">·</span>
                {t('dashboard.readableNoGraphSummary', { count: readableWithoutGraphCount })}
              </>
            )}
          </p>
        </div>
        <div className="flex flex-col items-start gap-2 sm:items-end">
          <StatusBadge tone={toStatusTone(graphStatusClass(graph.status))}>
            {t(`graph.statusLabels.${graph.status}`)}
          </StatusBadge>
          <span className="text-xs text-muted-foreground">
            {t('dashboard.updated')}: {formatDateTime(graph.updatedAt, locale, emptyLabel)}
          </span>
        </div>
      </div>

      <div className="mt-4 space-y-3">
        {visibleHealthRows.map((row) => {
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

      {/* Graph stat tiles — honest affordance: these route to /graph rather
          than looking clickable while doing nothing (DSH-04). */}
      <div className="mt-4 hidden grid-cols-3 gap-2 border-t pt-3 sm:grid">
        {[
          { label: t('dashboard.nodes'), value: graph.nodeCount, icon: Share2 },
          { label: t('dashboard.edges'), value: graph.edgeCount, icon: Activity },
          {
            label: t('dashboard.factDocs'),
            value: graph.typedFactDocumentCount,
            icon: Database,
          },
        ].map((item) => (
          <button
            key={item.label}
            type="button"
            onClick={() => onNavigate('/graph')}
            className="flex w-full items-start gap-2 rounded-lg bg-surface-sunken px-3 py-2 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
          >
            <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted">
              <item.icon className="h-3.5 w-3.5 text-muted-foreground" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-base font-semibold tabular-nums">
                {item.value}
              </div>
              <div className="text-xs font-medium leading-4 text-muted-foreground">
                {item.label}
              </div>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}

export const LibraryHealthPanel = memo(LibraryHealthPanelImpl);
