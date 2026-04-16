import { useState } from 'react';
import type { TFunction } from 'i18next';
import { Globe, Loader2, RotateCw } from 'lucide-react';
import { documentsApi, type WebIngestRunListItem, type WebIngestRunPageItem } from '@/api';
import { Button } from '@/components/ui/button';

const TERMINAL_RUN_STATES = new Set(['completed', 'completed_partial', 'failed']);

function runStatusClass(state: string): string {
  if (state === 'completed') return 'status-ready';
  if (state === 'failed') return 'status-failed';
  return 'status-processing';
}

function pageStateClass(state: string | undefined): string {
  if (state === 'processed') return 'bg-green-500';
  if (state === 'failed') return 'bg-red-500';
  if (state === 'excluded') return 'bg-yellow-500';
  return 'bg-gray-400';
}

function humanizeRunMode(mode: string, t: TFunction): string {
  if (mode === 'single_page') return t('documents.singlePage');
  if (mode === 'recursive_crawl') return t('documents.recursiveCrawl');
  return mode;
}

type WebRunsPanelProps = {
  t: TFunction;
  webRuns: WebIngestRunListItem[];
  onReuseRun: (run: WebIngestRunListItem) => void;
};

export function WebRunsPanel({ t, webRuns, onReuseRun }: WebRunsPanelProps) {
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runPages, setRunPages] = useState<WebIngestRunPageItem[]>([]);

  const handleToggleRun = async (runId: string) => {
    if (expandedRunId === runId) {
      setExpandedRunId(null);
      setRunPages([]);
      return;
    }
    setExpandedRunId(runId);
    try {
      setRunPages(await documentsApi.listWebRunPages(runId));
    } catch {
      setRunPages([]);
    }
  };

  const activeRuns = webRuns.filter(
    (r) => !TERMINAL_RUN_STATES.has(r.runState?.toLowerCase() ?? ''),
  );

  if (webRuns.length === 0) {
    return (
      <div className="empty-state py-20">
        <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
          <Globe className="h-7 w-7 text-muted-foreground" />
        </div>
        <h2 className="text-base font-bold tracking-tight">{t('documents.webIngestRuns')}</h2>
        <p className="text-sm text-muted-foreground mt-2">{t('documents.noDocsDesc')}</p>
      </div>
    );
  }

  return (
    <>
      {activeRuns.length > 0 && (
        <div className="mx-4 mt-4 flex items-center gap-2 text-xs px-3 py-2 rounded-xl bg-card border shadow-soft">
          <Loader2 className="h-3 w-3 animate-spin text-primary" />
          <span className="font-semibold">
            {t('documents.webRunActiveSummary', { count: activeRuns.length })}
          </span>
        </div>
      )}
      <div className="m-4 border rounded-xl">
        <div className="px-4 py-3 border-b flex items-center gap-2">
          <Globe className="h-4 w-4 text-muted-foreground" />
          <span className="text-sm font-semibold">{t('documents.webIngestRuns')}</span>
          <span className="text-xs text-muted-foreground ml-auto">{webRuns.length}</span>
        </div>
        <div className="divide-y">
          {webRuns.slice(0, 10).map((run) => (
            <div key={run.runId}>
              <button
                className="w-full px-4 py-2.5 flex items-center gap-3 text-left hover:bg-accent/30 transition-colors text-xs"
                onClick={() => void handleToggleRun(run.runId)}
              >
                <span className={`status-badge ${runStatusClass(run.runState)}`}>
                  {run.runState}
                </span>
                <span className="truncate font-medium" title={run.seedUrl}>
                  {run.seedUrl}
                </span>
                <span className="text-muted-foreground shrink-0">
                  {humanizeRunMode(run.mode, t)}
                </span>
                {run.mode === 'recursive_crawl' && (
                  <span className="text-muted-foreground shrink-0">
                    {t('documents.maxDepth')}: {run.maxDepth} · {t('documents.maxPages')}:{' '}
                    {run.maxPages}
                  </span>
                )}
                <span className="text-muted-foreground shrink-0">
                  {run.counts?.processed ?? 0}/{run.counts?.discovered ?? 0}{' '}
                  {t('documents.pages')}
                </span>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 shrink-0 ml-auto"
                  onClick={(e) => {
                    e.stopPropagation();
                    onReuseRun(run);
                  }}
                >
                  <RotateCw className="h-3 w-3" />
                </Button>
              </button>
              {expandedRunId === run.runId && runPages.length > 0 && (
                <div className="bg-muted/30 px-4 py-2 space-y-1">
                  {runPages.map((page, i) => (
                    <div key={i} className="flex items-center gap-2 text-[11px]">
                      <span
                        className={`w-1.5 h-1.5 rounded-full shrink-0 ${pageStateClass(
                          page.candidateState,
                        )}`}
                      />
                      <span
                        className="truncate text-muted-foreground"
                        title={page.normalizedUrl ?? page.discoveredUrl ?? '?'}
                      >
                        {page.normalizedUrl ?? page.discoveredUrl ?? '?'}
                      </span>
                      <span className="text-[10px] text-muted-foreground shrink-0">
                        {page.candidateState}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      </div>
    </>
  );
}
