import { useMemo } from 'react';
import type { TFunction } from 'i18next';
import {
  AlertCircle,
  CheckCircle2,
  Cpu,
  Layers,
  Loader2,
  MessageSquare,
  Sparkles,
  Wrench,
} from 'lucide-react';

import type { LlmContextDebugResponse } from '@/shared/api/query';
import type { EvidenceBundle } from '@/shared/types';

type McpToolsPanelProps = {
  t: TFunction;
  snapshot: LlmContextDebugResponse | null;
  loading: boolean;
  evidence: EvidenceBundle | null;
};

function formatDuration(ms: number | null | undefined) {
  if (ms == null) return '—';
  if (ms <= 0) return '<1 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function truncate(text: string, max: number) {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}…`;
}

function pickNumber(record: Record<string, unknown> | null, ...keys: string[]): number | null {
  if (!record) return null;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return null;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

export function McpToolsPanel({ t, snapshot, loading, evidence }: McpToolsPanelProps) {
  const stagesRaw = evidence?.runtimeSummary?.stages ?? [];
  const stagesAggregated = useMemo(() => {
    const map = new Map<string, { stage: string; durationMs: number; itemCount: number; calls: number }>();
    for (const stage of stagesRaw) {
      const existing = map.get(stage.stage);
      if (existing) {
        existing.durationMs += stage.durationMs ?? 0;
        existing.itemCount += stage.itemCount ?? 0;
        existing.calls += 1;
      } else {
        map.set(stage.stage, {
          stage: stage.stage,
          durationMs: stage.durationMs ?? 0,
          itemCount: stage.itemCount ?? 0,
          calls: 1,
        });
      }
    }
    return Array.from(map.values());
  }, [stagesRaw]);
  const totalStageMs = stagesAggregated.reduce((sum, stage) => sum + stage.durationMs, 0);

  const iterations = (snapshot?.iterations ?? []).map((iter, index) => ({
    ...iter,
    displayIndex: index + 1,
  }));
  const totalToolCalls = iterations.reduce(
    (sum, iter) => sum + iter.responseToolCalls.length,
    0,
  );
  const summary = evidence?.runtimeSummary;
  const hasContent =
    stagesAggregated.length > 0 || iterations.length > 0 || Boolean(summary);

  const finalAnswer = snapshot?.finalAnswer ?? null;
  const lastIterationResponse = iterations[iterations.length - 1]?.responseText ?? null;
  const showFinalAnswer = Boolean(
    finalAnswer && finalAnswer.trim() && finalAnswer.trim() !== (lastIterationResponse ?? '').trim(),
  );

  return (
    <aside className="hidden w-80 shrink-0 animate-slide-in-right overflow-y-auto border-l border-border/70 bg-card md:flex md:flex-col lg:w-96">
      <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-border/70 bg-card px-4 py-3">
        <Wrench className="h-4 w-4 text-primary" />
        <h3 className="text-sm font-bold tracking-tight">
          {t('assistant.mcpToolsTitle')}
        </h3>
        {(snapshot || evidence) && (
          <span className="ml-auto text-xs font-mono text-muted-foreground tabular-nums">
            {iterations.length + stagesAggregated.length}
          </span>
        )}
      </div>

      {loading && (
        <div className="flex flex-col items-center justify-center gap-2 px-4 py-10 text-sm text-muted-foreground">
          <Loader2 className="h-5 w-5 animate-spin text-primary/70" />
          {t('assistant.mcpToolsLoading')}
        </div>
      )}

      {!loading && !snapshot && !evidence && (
        <div className="px-4 py-10 text-center text-sm text-muted-foreground">
          {t('assistant.mcpToolsEmpty')}
        </div>
      )}

      {!loading && !hasContent && (snapshot || evidence) && (
        <div className="px-4 py-10 text-center text-sm text-muted-foreground">
          {t('assistant.mcpToolsNoCalls')}
        </div>
      )}

      {!loading && (snapshot || summary) && (
        <div className="border-b border-border/70 bg-surface-sunken px-4 py-3">
          {snapshot?.question && (
            <div className="mb-2 flex items-start gap-2 text-[11px]">
              <MessageSquare className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
              <span className="line-clamp-2 text-foreground" title={snapshot.question}>
                {snapshot.question}
              </span>
            </div>
          )}
          <div className="grid grid-cols-2 gap-2 text-[11px]">
            <Stat
              label={t('assistant.mcpStatStages')}
              value={stagesAggregated.length}
            />
            {snapshot && (
              <Stat
                label={t('assistant.mcpStatIterations')}
                value={snapshot.totalIterations}
              />
            )}
            {totalToolCalls > 0 && (
              <Stat
                label={t('assistant.mcpStatTools')}
                value={totalToolCalls}
              />
            )}
            {summary && (
              <>
                <Stat label={t('assistant.segmentRefs')} value={summary.totalSegments} />
                <Stat label={t('assistant.factRefs')} value={summary.totalFacts} />
                <Stat label={t('assistant.entityRefs')} value={summary.totalEntities} />
                <Stat
                  label={t('assistant.relationRefs')}
                  value={summary.totalRelations}
                />
              </>
            )}
          </div>
          {snapshot?.executionId && (
            <div
              className="mt-2 truncate font-mono text-[10px] text-muted-foreground"
              title={snapshot.executionId}
            >
              exec: {snapshot.executionId}
            </div>
          )}
        </div>
      )}

      {!loading && hasContent && (
        <div className="flex flex-col gap-3 p-4">
          <div className="section-label">
            {t('assistant.mcpTimelineTitle')}
          </div>

          {stagesAggregated.length === 0 && iterations.length > 0 && (
            <div className="rounded-md border border-status-warning/25 bg-status-warning/5 p-3 text-[11px] leading-relaxed text-status-warning">
              {t('assistant.mcpNoStageTelemetry')}
            </div>
          )}

          {/* Pipeline stages — runtime tools that IronRAG actually invoked */}
          {stagesAggregated.length > 0 && (
            <TimelineStep
              icon={<Layers className="h-3.5 w-3.5" />}
              tone="primary"
              order="P"
              title={t('assistant.mcpTimelinePipelineTitle')}
              meta={t('assistant.mcpTimelinePipelineMeta', {
                count: stagesAggregated.length,
                duration: formatDuration(totalStageMs),
              })}
            >
              <div className="mt-2 flex flex-col gap-1.5">
                {stagesAggregated.map(stage => {
                  const share = totalStageMs > 0 ? Math.round((stage.durationMs / totalStageMs) * 100) : 0;
                  return (
                    <div key={stage.stage} className="rounded-md border border-border/70 bg-background/60 px-2 py-1.5">
                      <div className="flex items-center gap-2">
                        <CheckCircle2 className="h-3 w-3 shrink-0 text-status-ready" />
                        <code className="truncate font-mono text-[11px] font-bold" title={stage.stage}>
                          {stage.stage}
                        </code>
                        {stage.calls > 1 && (
                          <span className="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 font-mono text-[10px] font-bold text-primary tabular-nums">
                            ×{stage.calls}
                          </span>
                        )}
                        <span className="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
                          {formatDuration(stage.durationMs)}
                        </span>
                      </div>
                      {(stage.itemCount > 0 || totalStageMs > 0) && (
                        <div className="mt-1 flex items-center justify-between gap-2 text-[10px] text-muted-foreground tabular-nums">
                          {stage.itemCount > 0 ? (
                            <span>
                              {t('assistant.mcpStageItems', {
                                count: stage.itemCount,
                                })}
                            </span>
                          ) : (
                            <span />
                          )}
                          {totalStageMs > 0 && <span>{share}%</span>}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </TimelineStep>
          )}

          {/* LLM iterations — chronologically, each tool-call shown right under its iteration */}
          {iterations.map(iter => {
            const userMsgs = iter.requestMessages.filter(m => m.role === 'user').length;
            const sysMsgs = iter.requestMessages.filter(m => m.role === 'system').length;
            const usage = asRecord(iter.usage);
            const promptTokens = pickNumber(usage, 'promptTokens', 'prompt_tokens', 'input_tokens');
            const completionTokens = pickNumber(usage, 'completionTokens', 'completion_tokens', 'output_tokens');
            const totalTokens = pickNumber(usage, 'totalTokens', 'total_tokens')
              ?? ((promptTokens ?? 0) + (completionTokens ?? 0) || null);
            const responsePreview = iter.responseText ? truncate(iter.responseText.trim(), 240) : '';
            return (
              <TimelineStep
                key={`iter-${iter.displayIndex}-${iter.iteration}`}
                icon={<Cpu className="h-3.5 w-3.5" />}
                tone="iteration"
                order={String(iter.displayIndex)}
                title={
                  <span className="flex items-center gap-2 truncate">
                    <span className="truncate font-mono text-xs font-semibold">
                      {iter.modelName}
                    </span>
                    <span className="shrink-0 text-[10px] text-muted-foreground">
                      @ {iter.providerKind}
                    </span>
                  </span>
                }
                meta={t('assistant.mcpIterationMeta', {
                  sys: sysMsgs,
                  usr: userMsgs,
                  tools: iter.responseToolCalls.length,
                })}
              >
                {totalTokens != null && (
                  <div className="mt-2 flex items-center gap-2 rounded-md border border-border/70 bg-background/60 px-2 py-1.5 text-[10px] tabular-nums text-muted-foreground">
                    <span className="font-semibold uppercase tracking-wide">tokens</span>
                    {promptTokens != null && <span>in {promptTokens}</span>}
                    {completionTokens != null && <span>out {completionTokens}</span>}
                    <span className="ml-auto font-mono font-semibold text-foreground">{totalTokens}</span>
                  </div>
                )}
                {iter.responseToolCalls.length > 0 && (
                  <div className="mt-2 flex flex-col gap-1.5">
                    {iter.responseToolCalls.map(tc => (
                      <article
                        key={tc.id}
                        className={`rounded-md border px-2 py-1.5 text-[11px] ${
                          tc.isError
                            ? 'border-status-failed/30 bg-status-failed/5'
                            : 'border-border/70 bg-background/60'
                        }`}
                      >
                        <header className="flex items-center gap-2">
                          {tc.isError ? (
                            <AlertCircle className="h-3 w-3 shrink-0 text-status-failed" />
                          ) : (
                            <Wrench className="h-3 w-3 shrink-0 text-primary" />
                          )}
                          <code className="truncate font-mono text-[11px] font-bold" title={tc.name}>
                            {tc.name}
                          </code>
                        </header>
                        {tc.argumentsJson && tc.argumentsJson !== '{}' && (
                          <details className="mt-1.5">
                            <summary className="cursor-pointer text-[10px] text-muted-foreground hover:text-foreground">
                              {t('assistant.mcpToolsArgs')}
                            </summary>
                            <pre className="mt-1 max-h-40 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
                              {tc.argumentsJson}
                            </pre>
                          </details>
                        )}
                        {tc.resultText && (
                          <details className="mt-1">
                            <summary className="cursor-pointer text-[10px] text-muted-foreground hover:text-foreground">
                              {t('assistant.mcpToolsResult')}
                            </summary>
                            <pre className="mt-1 max-h-40 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
                              {tc.resultText}
                            </pre>
                          </details>
                        )}
                      </article>
                    ))}
                  </div>
                )}
                {responsePreview && (
                  <details className="mt-2">
                    <summary className="cursor-pointer text-[11px] font-medium text-muted-foreground hover:text-foreground">
                      {t('assistant.mcpIterationResponse')}
                    </summary>
                    <pre className="mt-1.5 max-h-40 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
                      {iter.responseText}
                    </pre>
                  </details>
                )}
              </TimelineStep>
            );
          })}

          {showFinalAnswer && finalAnswer && (
            <TimelineStep
              icon={<Sparkles className="h-3.5 w-3.5" />}
              tone="success"
              order="✓"
              title={t('assistant.mcpFinalAnswer')}
            >
              <details className="mt-2">
                <summary className="cursor-pointer text-[11px] font-medium text-muted-foreground hover:text-foreground">
                  {t('assistant.mcpFinalAnswerExpand')}
                </summary>
                <pre className="mt-1.5 max-h-60 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
                  {finalAnswer}
                </pre>
              </details>
            </TimelineStep>
          )}
        </div>
      )}
    </aside>
  );
}

function Stat({ label, value }: { label: string; value: number | string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="font-mono text-sm font-semibold tabular-nums">{value}</span>
    </div>
  );
}

type TimelineStepProps = {
  icon: React.ReactNode;
  tone: 'primary' | 'iteration' | 'success';
  order: string;
  title: React.ReactNode;
  meta?: string;
  children?: React.ReactNode;
};

function TimelineStep({ icon, tone, order, title, meta, children }: TimelineStepProps) {
  const toneClasses = {
    primary: 'border-primary/40 bg-primary/5 text-primary',
    iteration: 'border-border bg-card text-foreground',
    success: 'border-status-ready/40 bg-status-ready/5 text-status-ready',
  } as const;
  return (
    <div className="relative pl-8">
      <span
        className={`absolute left-0 top-0 flex h-6 w-6 items-center justify-center rounded-full border ${toneClasses[tone]} text-[10px] font-bold tabular-nums`}
      >
        {order}
      </span>
      <div className="rounded-md border border-border/70 bg-card p-3 shadow-sm">
        <header className="flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">{icon}</span>
          <div className="min-w-0 flex-1 truncate text-sm font-semibold">{title}</div>
        </header>
        {meta && (
          <div className="mt-1 text-[10px] tabular-nums text-muted-foreground">{meta}</div>
        )}
        {children}
      </div>
    </div>
  );
}
