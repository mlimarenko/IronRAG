import {
  useCallback,
  useMemo,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';
import type { TFunction } from 'i18next';
import {
  AlertCircle,
  Bug,
  CheckCircle2,
  Cpu,
  FileJson,
  GripVertical,
  Layers,
  Loader2,
  MessageSquare,
  Sparkles,
  Wrench,
  X,
} from 'lucide-react';

import type { LlmContextDebugResponse } from '@/shared/api/query';
import type { EvidenceBundle } from '@/shared/types';

const DEBUG_PANEL_MIN_WIDTH = 320;
const DEBUG_PANEL_MAX_WIDTH = 720;

type AssistantDebugInspectorProps = {
  t: TFunction;
  open: boolean;
  width: number;
  snapshot: LlmContextDebugResponse | null;
  loading: boolean;
  error: string | null;
  evidence: EvidenceBundle | null;
  onClose: () => void;
  onWidthChange: (width: number) => void;
};

function clampPanelWidth(width: number) {
  return Math.min(DEBUG_PANEL_MAX_WIDTH, Math.max(DEBUG_PANEL_MIN_WIDTH, Math.round(width)));
}

function formatDuration(ms: number | null | undefined) {
  if (ms == null) return '-';
  if (ms <= 0) return '<1 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function truncate(text: string, max: number) {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}...`;
}

function stringifyJson(value: unknown): string {
  if (value == null) return 'null';
  return JSON.stringify(value, null, 2);
}

function hasJsonPayload(value: unknown): boolean {
  if (value == null) return false;
  if (typeof value !== 'object') return true;
  if (Array.isArray(value)) return value.length > 0;
  return Object.keys(value).length > 0;
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

export function AssistantDebugInspector({
  t,
  open,
  width,
  snapshot,
  loading,
  error,
  evidence,
  onClose,
  onWidthChange,
}: AssistantDebugInspectorProps) {
  const panelWidth = clampPanelWidth(width);
  const panelStyle = {
    '--assistant-debug-width': `${panelWidth}px`,
  } as CSSProperties;

  const startResize = useCallback(
    (event: PointerEvent<HTMLDivElement>) => {
      if (event.pointerType === 'mouse' && event.button !== 0) return;
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = panelWidth;
      const handleMove = (moveEvent: globalThis.PointerEvent) => {
        onWidthChange(clampPanelWidth(startWidth + startX - moveEvent.clientX));
      };
      const handleUp = () => {
        window.removeEventListener('pointermove', handleMove);
        window.removeEventListener('pointerup', handleUp);
      };
      window.addEventListener('pointermove', handleMove);
      window.addEventListener('pointerup', handleUp);
    },
    [onWidthChange, panelWidth],
  );

  const handleResizeKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key === 'ArrowLeft') {
        event.preventDefault();
        onWidthChange(clampPanelWidth(panelWidth + 16));
      } else if (event.key === 'ArrowRight') {
        event.preventDefault();
        onWidthChange(clampPanelWidth(panelWidth - 16));
      } else if (event.key === 'Home') {
        event.preventDefault();
        onWidthChange(DEBUG_PANEL_MIN_WIDTH);
      } else if (event.key === 'End') {
        event.preventDefault();
        onWidthChange(DEBUG_PANEL_MAX_WIDTH);
      }
    },
    [onWidthChange, panelWidth],
  );

  const stagesAggregated = useMemo(() => {
    const stagesRaw = evidence?.runtimeSummary?.stages ?? [];
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
  }, [evidence?.runtimeSummary?.stages]);

  const totalStageMs = stagesAggregated.reduce((sum, stage) => sum + stage.durationMs, 0);
  const iterations = (snapshot?.iterations ?? []).map((iter, index) => ({
    ...iter,
    displayIndex: index + 1,
  }));
  const totalToolCalls = iterations.reduce(
    (sum, iter) => sum + iter.responseToolCalls.length,
    0,
  );
  const messageCount = iterations.reduce((sum, iter) => sum + iter.requestMessages.length, 0);
  const summary = evidence?.runtimeSummary;
  const finalAnswer = snapshot?.finalAnswer ?? null;
  const lastIterationResponse = iterations[iterations.length - 1]?.responseText ?? null;
  const showFinalAnswer = Boolean(
    finalAnswer && finalAnswer.trim() && finalAnswer.trim() !== (lastIterationResponse ?? '').trim(),
  );
  const hasContent =
    stagesAggregated.length > 0 ||
    iterations.length > 0 ||
    Boolean(summary) ||
    hasJsonPayload(snapshot?.queryIr) ||
    hasJsonPayload(snapshot?.agentLoop) ||
    Boolean(finalAnswer);

  if (!open) return null;

  return (
    <aside
      className="fixed inset-y-0 right-0 z-40 flex w-full flex-col border-l border-border/70 bg-card shadow-elevated md:relative md:inset-auto md:z-auto md:w-[var(--assistant-debug-width)] md:shrink-0 md:shadow-none"
      style={panelStyle}
      data-testid="assistant-debug-inspector"
    >
      <div
        role="separator"
        tabIndex={0}
        aria-orientation="vertical"
        aria-valuemin={DEBUG_PANEL_MIN_WIDTH}
        aria-valuemax={DEBUG_PANEL_MAX_WIDTH}
        aria-valuenow={panelWidth}
        className="absolute -left-2 top-0 hidden h-full w-4 cursor-col-resize items-center justify-center text-muted-foreground transition-colors hover:text-foreground md:flex"
        aria-label={t('assistant.debugInspectorResize')}
        onPointerDown={startResize}
        onKeyDown={handleResizeKeyDown}
      >
        <span className="rounded-full border border-border/70 bg-card p-0.5 shadow-soft">
          <GripVertical className="h-3.5 w-3.5" />
        </span>
      </div>

      <header className="flex shrink-0 items-center gap-2 border-b border-border/70 bg-card px-4 py-3">
        <Bug className="h-4 w-4 text-primary" />
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-sm font-bold tracking-tight">
            {t('assistant.debugInspectorTitle')}
          </h3>
          <div className="truncate font-mono text-[10px] text-muted-foreground">
            {snapshot?.executionId ?? t('assistant.debugInspectorNoContext')}
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          aria-label={t('assistant.close')}
        >
          <X className="h-4 w-4" />
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {loading && (
          <div className="flex flex-col items-center justify-center gap-2 px-4 py-10 text-sm text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin text-primary/70" />
            {t('assistant.debugInspectorLoading')}
          </div>
        )}

        {!loading && error && (
          <div className="m-4 rounded-md border border-status-failed/30 bg-status-failed/5 p-3 text-sm text-status-failed">
            <div className="flex items-start gap-2">
              <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
              <div>{error}</div>
            </div>
          </div>
        )}

        {!loading && !error && !snapshot && !evidence && (
          <div className="px-4 py-10 text-center text-sm leading-relaxed text-muted-foreground">
            {t('assistant.debugInspectorEmpty')}
          </div>
        )}

        {!loading && !error && !hasContent && (snapshot || evidence) && (
          <div className="px-4 py-10 text-center text-sm leading-relaxed text-muted-foreground">
            {t('assistant.debugInspectorNoContent')}
          </div>
        )}

        {!loading && !error && (snapshot || summary) && (
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
              <Stat label={t('assistant.mcpStatStages')} value={stagesAggregated.length} />
              {snapshot && <Stat label={t('assistant.mcpStatIterations')} value={snapshot.totalIterations} />}
              <Stat label={t('assistant.llmContextMessages')} value={messageCount} />
              <Stat label={t('assistant.mcpStatTools')} value={totalToolCalls} />
              {summary && (
                <>
                  <Stat label={t('assistant.segmentRefs')} value={summary.totalSegments} />
                  <Stat label={t('assistant.factRefs')} value={summary.totalFacts} />
                  <Stat label={t('assistant.entityRefs')} value={summary.totalEntities} />
                  <Stat label={t('assistant.relationRefs')} value={summary.totalRelations} />
                </>
              )}
            </div>
          </div>
        )}

        {!loading && !error && hasContent && (
          <div className="flex flex-col gap-3 p-4">
            <div className="section-label">{t('assistant.mcpTimelineTitle')}</div>

            {stagesAggregated.length === 0 && iterations.length > 0 && (
              <div className="rounded-md border border-status-warning/25 bg-status-warning/5 p-3 text-[11px] leading-relaxed text-status-warning">
                {t('assistant.mcpNoStageTelemetry')}
              </div>
            )}

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
                              x{stage.calls}
                            </span>
                          )}
                          <span className="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
                            {formatDuration(stage.durationMs)}
                          </span>
                        </div>
                        {(stage.itemCount > 0 || totalStageMs > 0) && (
                          <div className="mt-1 flex items-center justify-between gap-2 text-[10px] text-muted-foreground tabular-nums">
                            {stage.itemCount > 0 ? (
                              <span>{t('assistant.mcpStageItems', { count: stage.itemCount })}</span>
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
                            <JsonDetails label={t('assistant.mcpToolsArgs')} value={tc.argumentsJson} />
                          )}
                          {tc.resultText && (
                            <JsonDetails label={t('assistant.mcpToolsResult')} value={tc.resultText} />
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

            {iterations.length > 0 && (
              <>
                <div className="section-label mt-2">{t('assistant.requestMessages')}</div>
                {iterations.map(iter => (
                  <details key={`messages-${iter.iteration}`} className="rounded-md border border-border/70 bg-card">
                    <summary className="cursor-pointer px-3 py-2 text-xs font-semibold">
                      {t('assistant.iteration')} #{iter.iteration} · {iter.requestMessages.length}
                    </summary>
                    <div className="space-y-2 border-t border-border/60 p-3">
                      {iter.requestMessages.map((message, index) => (
                        <article key={`${message.role}-${index}`} className="rounded-md border border-border/60 bg-background/60 p-2">
                          <div className="mb-1 flex items-center gap-2">
                            <span className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-[10px] font-bold uppercase text-primary">
                              {message.role}
                            </span>
                            {message.tool_call_id && (
                              <span className="truncate font-mono text-[10px] text-muted-foreground">
                                {t('assistant.toolCallIdLabel')}: {message.tool_call_id}
                              </span>
                            )}
                          </div>
                          {message.content && (
                            <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-foreground/80">
                              {message.content}
                            </pre>
                          )}
                          {message.tool_calls && message.tool_calls.length > 0 && (
                            <div className="mt-2 space-y-1">
                              {message.tool_calls.map(toolCall => (
                                <div key={toolCall.id} className="rounded border border-status-warning/30 bg-status-warning/5 p-2 font-mono text-[10px] text-status-warning">
                                  {toolCall.name}({toolCall.arguments_json})
                                </div>
                              ))}
                            </div>
                          )}
                        </article>
                      ))}
                    </div>
                  </details>
                ))}
              </>
            )}

            {hasJsonPayload(snapshot?.queryIr) && (
              <RawJsonBlock icon={<FileJson className="h-3.5 w-3.5" />} title={t('assistant.queryIr')} value={snapshot?.queryIr} />
            )}

            {hasJsonPayload(snapshot?.agentLoop) && (
              <RawJsonBlock icon={<FileJson className="h-3.5 w-3.5" />} title={t('assistant.agentLoop')} value={snapshot?.agentLoop} />
            )}

            {showFinalAnswer && finalAnswer && (
              <TimelineStep
                icon={<Sparkles className="h-3.5 w-3.5" />}
                tone="success"
                order="OK"
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
      </div>
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

function JsonDetails({ label, value }: { label: string; value: string }) {
  return (
    <details className="mt-1.5">
      <summary className="cursor-pointer text-[10px] text-muted-foreground hover:text-foreground">
        {label}
      </summary>
      <pre className="mt-1 max-h-40 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
        {value}
      </pre>
    </details>
  );
}

function RawJsonBlock({ icon, title, value }: { icon: ReactNode; title: string; value: unknown }) {
  return (
    <details className="rounded-md border border-border/70 bg-card">
      <summary className="flex cursor-pointer items-center gap-2 px-3 py-2 text-xs font-semibold">
        <span className="text-muted-foreground">{icon}</span>
        {title}
      </summary>
      <pre className="max-h-72 overflow-auto border-t border-border/60 p-3 font-mono text-[10px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]">
        {stringifyJson(value)}
      </pre>
    </details>
  );
}

type TimelineStepProps = {
  icon: ReactNode;
  tone: 'primary' | 'iteration' | 'success';
  order: string;
  title: ReactNode;
  meta?: string;
  children?: ReactNode;
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
        className={`absolute left-0 top-0 flex h-6 min-w-6 items-center justify-center rounded-full border px-1 ${toneClasses[tone]} text-[10px] font-bold tabular-nums`}
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
