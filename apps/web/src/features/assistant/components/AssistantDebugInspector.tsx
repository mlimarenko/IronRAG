import {
  useCallback,
  useMemo,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';
import type { TFunction } from 'i18next';
import { useQuery } from '@tanstack/react-query';
import {
  AlertCircle,
  Braces,
  Bug,
  ChevronDown,
  ChevronRight,
  Cpu,
  FileJson,
  GripVertical,
  Layers,
  ListTree,
  Loader2,
  MessageSquare,
  ScrollText,
  Wrench,
  X,
} from 'lucide-react';

import { queryApi } from '@/shared/api';
import type { LlmContextDebugResponse } from '@/shared/api/query';
import type { EvidenceBundle } from '@/shared/types';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/shared/components/ui/tooltip';

const DEBUG_PANEL_MIN_WIDTH = 420;
const DEBUG_PANEL_MAX_WIDTH = 960;

type InspectorView = 'timeline' | 'context' | 'raw';

type ContextToolCallPreview = {
  id: string;
  name: string;
  argumentsJson: string;
  isError: boolean;
};

type ContextTranscriptPhase = 'request' | 'model' | 'tool' | 'final';

type ContextTranscriptEntry = {
  key: string;
  role: string;
  phase: ContextTranscriptPhase;
  content?: string | null;
  toolCallId?: string | null;
  toolName?: string | null;
  toolCalls?: ContextToolCallPreview[];
  resultJson?: unknown;
  isError?: boolean;
};

type ContextTranscriptSection = {
  iteration: number;
  entries: ContextTranscriptEntry[];
};

type AssistantDebugInspectorProps = {
  t: TFunction;
  open: boolean;
  width: number;
  snapshot: LlmContextDebugResponse | null;
  loading: boolean;
  error: string | null;
  evidence: EvidenceBundle | null;
  turnWallClockMs?: number;
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

function formatJsonish(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return '';
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return trimmed;
  }
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
  turnWallClockMs,
  onClose,
  onWidthChange,
}: AssistantDebugInspectorProps) {
  const panelWidth = clampPanelWidth(width);
  const [activeView, setActiveView] = useState<InspectorView>('timeline');
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
  const totalModelMs = iterations.reduce((sum, iter) => sum + (iter.durationMs ?? 0), 0);
  const totalToolMs = iterations.reduce(
    (sum, iter) =>
      sum + iter.responseToolCalls.reduce((inner, tc) => inner + (tc.durationMs ?? 0), 0),
    0,
  );
  const maxIterationSpanMs = Math.max(
    1,
    ...iterations.map(
      (iter) =>
        (iter.durationMs ?? 0) +
        iter.responseToolCalls.reduce((inner, tc) => inner + (tc.durationMs ?? 0), 0),
    ),
  );
  const totalTurnMs = Math.max(totalStageMs, totalModelMs + totalToolMs);
  const summary = evidence?.runtimeSummary;
  const finalAnswer = snapshot?.finalAnswer ?? null;
  const lastIterationResponse = iterations[iterations.length - 1]?.responseText ?? null;
  const latestIteration = iterations[iterations.length - 1] ?? null;
  const contextSections: ContextTranscriptSection[] = latestIteration ? (() => {
    const entries: ContextTranscriptEntry[] = latestIteration.requestMessages.map((message, index) => ({
      key: `request-${latestIteration.iteration}-${index}`,
      role: message.role,
      phase: 'request',
      content: message.content,
      toolCallId: message.tool_call_id,
      toolName: message.name,
      toolCalls: message.tool_calls?.map(toolCall => ({
        id: toolCall.id,
        name: toolCall.name,
        argumentsJson: toolCall.arguments_json,
        isError: false,
      })),
    }));

    if (latestIteration.responseToolCalls.length > 0) {
      entries.push({
        key: `assistant-response-${latestIteration.iteration}`,
        role: 'assistant',
        phase: 'model',
        content: latestIteration.responseText,
        toolCalls: latestIteration.responseToolCalls.map(toolCall => ({
          id: toolCall.id,
          name: toolCall.name,
          argumentsJson: toolCall.argumentsJson,
          isError: toolCall.isError,
        })),
      });
    }

    for (const [index, toolCall] of latestIteration.responseToolCalls.entries()) {
      entries.push({
        key: `tool-result-${latestIteration.iteration}-${toolCall.id || toolCall.name}-${index}`,
        role: 'tool',
        phase: 'tool',
        content: toolCall.resultText,
        toolCallId: toolCall.id,
        toolName: toolCall.name,
        resultJson: toolCall.resultJson,
        isError: toolCall.isError,
      });
    }

    return [{
      iteration: latestIteration.iteration,
      entries,
    }];
  })() : [];
  const transcriptFinalAnswer = finalAnswer?.trim() ? finalAnswer : lastIterationResponse;
  const transcriptFinalAnswerText = transcriptFinalAnswer?.trim() ?? '';
  const hasMatchingAssistantContextEntry = contextSections.some(section =>
    section.entries.some(entry =>
      entry.role === 'assistant' &&
      (entry.content ?? '').trim() === transcriptFinalAnswerText,
    ),
  );
  const showFinalAnswerInContext = Boolean(
    transcriptFinalAnswerText && !hasMatchingAssistantContextEntry,
  );
  const contextEntryCount = contextSections.reduce(
    (sum, section) => sum + section.entries.length,
    showFinalAnswerInContext ? 1 : 0,
  );
  const rawBlockCount = [
    hasJsonPayload(snapshot?.queryIr),
    hasJsonPayload(snapshot?.agentLoop),
  ].filter(Boolean).length;
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
      className="fixed inset-y-0 right-0 z-40 flex w-full flex-col border-l border-border/70 bg-background shadow-elevated md:relative md:inset-auto md:z-auto md:w-[var(--assistant-debug-width)] md:shrink-0 md:shadow-none"
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

      <TooltipProvider delayDuration={0} skipDelayDuration={0}>
        <header className="flex shrink-0 items-start gap-3 border-b border-border/70 bg-card/95 px-4 py-3 backdrop-blur">
          <div className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-primary/20 bg-primary/10 text-primary">
            <Bug className="h-4 w-4" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h3 className="truncate text-sm font-bold tracking-tight">
                {t('assistant.debugInspectorTitle')}
              </h3>
              {snapshot?.agentLoop && (
                <span className="shrink-0 rounded-md border border-border/70 bg-background px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
                  {t('assistant.agentLoop')}
                </span>
              )}
            </div>
            <div className="mt-0.5 truncate font-mono text-[10px] text-muted-foreground">
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
                <div className="mb-3 rounded-md border border-border/70 bg-card px-3 py-2 text-[11px]">
                  <div className="mb-1 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
                    <MessageSquare className="h-3 w-3" />
                    {t('assistant.debugQuestion')}
                  </div>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <span className="line-clamp-3 cursor-default text-foreground">
                        {snapshot.question}
                      </span>
                    </TooltipTrigger>
                    <TooltipContent>{snapshot.question}</TooltipContent>
                  </Tooltip>
                </div>
              )}
              <div className="grid grid-cols-2 gap-2 text-[11px] sm:grid-cols-4">
                {(Number.isFinite(turnWallClockMs) && (turnWallClockMs ?? 0) > 0 ? true : totalTurnMs > 0) && (
                  <Stat label={t('assistant.debugTotalTime')} value={formatDuration(Number.isFinite(turnWallClockMs) && (turnWallClockMs ?? 0) > 0 ? (turnWallClockMs ?? null) : totalTurnMs)} />
                )}
                {snapshot && (
                  <Stat label={t('assistant.mcpStatIterations')} value={iterations.length} />
                )}
                <Stat label={t('assistant.mcpStatTools')} value={totalToolCalls} />
                <Stat label={t('assistant.mcpStatStages')} value={stagesAggregated.length} />
              </div>
              <div className="mt-3 grid grid-cols-3 gap-1 rounded-lg border border-border/70 bg-background p-1">
                <InspectorTab
                  active={activeView === 'timeline'}
                  icon={<ListTree className="h-3.5 w-3.5" />}
                  label={t('assistant.debugViewTimeline')}
                  count={iterations.length + (stagesAggregated.length > 0 ? 1 : 0)}
                  onClick={() => setActiveView('timeline')}
                />
                <InspectorTab
                  active={activeView === 'context'}
                  icon={<ScrollText className="h-3.5 w-3.5" />}
                  label={t('assistant.debugViewContext')}
                  count={contextEntryCount}
                  onClick={() => setActiveView('context')}
                />
                <InspectorTab
                  active={activeView === 'raw'}
                  icon={<Braces className="h-3.5 w-3.5" />}
                  label={t('assistant.debugViewRaw')}
                  count={rawBlockCount}
                  onClick={() => setActiveView('raw')}
                />
              </div>
            </div>
          )}

          {!loading && !error && hasContent && (
            <div className="flex flex-col gap-3 p-4">
              {activeView === 'timeline' && (
                <>
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
                      {totalStageMs > 0 && (
                        <div className="mt-2">
                          <StagePanel
                            segments={stagesAggregated.map((stage, index) => ({
                              key: stage.stage,
                              ms: stage.durationMs,
                              color: spanColor(index),
                              tip: stageTooltip(t, stage.stage),
                              calls: stage.calls,
                            }))}
                          />
                        </div>
                      )}
                    </TimelineStep>
                  )}

                  {iterations.length > 0 && (
                    <div className="section-label mt-1">{t('assistant.mcpModelCallsTitle')}</div>
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
                    const modelCallTooltip = t('assistant.tooltipModelCall', {
                      iteration: iter.displayIndex,
                      think: formatDuration(iter.durationMs),
                      tokIn: promptTokens ?? '-',
                      tokOut: completionTokens ?? '-',
                    });
                    return (
                      <TimelineStep
                        key={`iter-${iter.displayIndex}-${iter.iteration}`}
                        icon={<Cpu className="h-3.5 w-3.5" />}
                        tone="iteration"
                        order={String(iter.displayIndex)}
                        title={
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <span className="flex cursor-default items-center gap-2 truncate">
                                <span className="truncate font-mono text-xs font-semibold">
                                  {iter.modelName}
                                </span>
                                <span className="shrink-0 text-[10px] text-muted-foreground">
                                  @ {iter.providerKind}
                                </span>
                              </span>
                            </TooltipTrigger>
                            <TooltipContent>{modelCallTooltip}</TooltipContent>
                          </Tooltip>
                        }
                        meta={t('assistant.mcpIterationMeta', {
                          sys: sysMsgs,
                          usr: userMsgs,
                          tools: iter.responseToolCalls.length,
                        })}
                      >
                        {iter.durationMs != null && iter.durationMs > 0 && (
                          <div className="mt-2">
                            <div className="mb-1 flex items-center gap-2 text-[10px] tabular-nums text-muted-foreground">
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <span
                                    tabIndex={0}
                                    className="cursor-default font-semibold uppercase tracking-wide underline decoration-dotted decoration-muted-foreground/40 underline-offset-2 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring rounded"
                                  >
                                    {t('assistant.mcpModelThink')}
                                  </span>
                                </TooltipTrigger>
                                <TooltipContent>{t('assistant.stageTipModelThink')}</TooltipContent>
                              </Tooltip>
                              <span className="ml-auto font-mono font-semibold text-foreground">
                                {formatDuration(iter.durationMs)}
                              </span>
                            </div>
                            <DurationBar ms={iter.durationMs} maxMs={maxIterationSpanMs} color={spanColor(0)} />
                          </div>
                        )}
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
                            {iter.responseToolCalls.map((tc, tcIndex) => (
                              <ToolCallRow
                                key={`${iter.iteration}-${tc.id || tc.name}-${tcIndex}`}
                                t={t}
                                toolCall={tc}
                                maxIterationSpanMs={maxIterationSpanMs}
                                childExecutionId={
                                  iter.childQueryExecutionIds?.[tcIndex] ??
                                  (iter.responseToolCalls.length === 1
                                    ? iter.childQueryExecutionIds?.[0]
                                    : undefined)
                                }
                              />
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

                  {snapshot?.spans && snapshot.spans.length > 0 && (
                    <SpansSection t={t} spans={snapshot.spans} />
                  )}
                </>
              )}

              {activeView === 'context' && contextEntryCount > 0 && (
                <>
                  <div className="section-label mt-2">{t('assistant.requestMessages')}</div>
                  {contextSections.map(section => (
                    <details
                      key={`messages-${section.iteration}`}
                      className="rounded-md border border-border/70 bg-card"
                      open
                    >
                      <summary className="cursor-pointer px-3 py-2 text-xs font-semibold">
                        {t('assistant.iteration')} #{section.iteration} · {section.entries.length}
                      </summary>
                      <div className="space-y-2 border-t border-border/60 p-3">
                        {section.entries.map(entry => (
                          <ContextTranscriptCard
                            key={entry.key}
                            t={t}
                            entry={entry}
                          />
                        ))}
                      </div>
                    </details>
                  ))}
                  {showFinalAnswerInContext && transcriptFinalAnswer && (
                    <ContextTranscriptCard
                      t={t}
                      entry={{
                        key: 'final-answer',
                        role: 'assistant',
                        phase: 'final',
                        content: transcriptFinalAnswer,
                      }}
                    />
                  )}
                </>
              )}

              {activeView === 'context' && contextEntryCount === 0 && (
                <div className="rounded-lg border border-border/70 bg-card p-4 text-sm text-muted-foreground">
                  {t('assistant.mcpContextEmpty')}
                </div>
              )}

              {activeView === 'raw' && (
                <>
                  <div className="section-label">{t('assistant.debugViewRaw')}</div>

                  {snapshot?.agentLoop && <AgentLoopSummary t={t} agentLoop={snapshot.agentLoop} />}

                  {hasJsonPayload(snapshot?.queryIr) && (
                    <RawJsonBlock icon={<FileJson className="h-3.5 w-3.5" />} title={t('assistant.queryIr')} value={snapshot?.queryIr} />
                  )}

                  {hasJsonPayload(snapshot?.agentLoop) && (
                    <RawJsonBlock icon={<FileJson className="h-3.5 w-3.5" />} title={t('assistant.agentLoop')} value={snapshot?.agentLoop} />
                  )}
                </>
              )}
            </div>
          )}
        </div>
      </TooltipProvider>
    </aside>
  );
}

function stageTooltip(t: TFunction, stageKind: string): string | undefined {
  switch (stageKind) {
    case 'compile':
      return t('assistant.stageTipCompile');
    case 'retrieve':
      return t('assistant.stageTipRetrieve');
    case 'answer':
      return t('assistant.stageTipAnswer');
    case 'verify':
      return t('assistant.stageTipVerify');
    case 'persist':
      return t('assistant.stageTipPersist');
    default:
      return undefined;
  }
}

function stoppedReasonLabel(t: TFunction, reason: string) {
  switch (reason) {
    case 'final_answer':
      return t('assistant.agentStopFinalAnswer');
    case 'iteration_cap':
      return t('assistant.agentStopIterationCap');
    case 'deadline':
      return t('assistant.agentStopDeadline');
    case 'tool_error':
      return t('assistant.agentStopToolError');
    case 'provider_error':
      return t('assistant.agentStopProviderError');
    default:
      return reason;
  }
}

/** Agent-loop metadata rendered as readable labeled fields instead of raw JSON. */
function AgentLoopSummary({
  t,
  agentLoop,
}: {
  t: TFunction;
  agentLoop: NonNullable<LlmContextDebugResponse['agentLoop']>;
}) {
  const fields: { label: string; value: string | number }[] = [
    { label: t('assistant.agentLoopStopped'), value: stoppedReasonLabel(t, agentLoop.stoppedReason) },
    { label: t('assistant.mcpStatTools'), value: agentLoop.toolCallCount },
    { label: t('assistant.agentLoopIterationCap'), value: agentLoop.iterationCap },
    { label: t('assistant.debugBudget'), value: formatDuration(agentLoop.deadlineMs) },
  ];
  return (
    <div className="overflow-hidden rounded-md border border-border/70 bg-card">
      <div className="flex items-center gap-2 border-b border-border/60 px-3 py-2 text-xs font-semibold">
        <Bug className="h-3.5 w-3.5 text-muted-foreground" />
        {t('assistant.agentLoop')}
      </div>
      <div className="grid grid-cols-2 gap-px bg-border/40">
        {fields.map((field) => (
          <div key={field.label} className="flex flex-col gap-0.5 bg-card px-3 py-2">
            <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
              {field.label}
            </span>
            <span className="font-mono text-xs font-semibold tabular-nums">{field.value}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

const SPAN_COLORS = ['#6366f1', '#0ea5e9', '#10b981', '#f59e0b', '#ef4444', '#8b5cf6', '#14b8a6'];

function spanColor(index: number) {
  return SPAN_COLORS[index % SPAN_COLORS.length];
}

function shareOf(ms: number, totalMs: number) {
  if (totalMs <= 0) return 0;
  return Math.round((ms / totalMs) * 100);
}

type ShareSegment = {
  key: string;
  ms: number;
  color: string;
  /** Human-readable description for the tooltip (falls back to `key`). */
  tip?: string;
  /** Repeat-call count, surfaced as an `x{n}` badge when > 1. */
  calls?: number;
};

/** Rich hover card for one stage: bold label + monospace duration · share. */
function StageTooltipContent({ segment, share }: { segment: ShareSegment; share: number }) {
  return (
    <TooltipContent>
      <div className="font-semibold text-popover-foreground">{segment.tip ?? segment.key}</div>
      <div className="mt-0.5 font-mono text-[11px] tabular-nums text-muted-foreground">
        {formatDuration(segment.ms)}
        {' · '}
        {share}%
        {segment.calls && segment.calls > 1 ? ` · x${segment.calls}` : ''}
      </div>
    </TooltipContent>
  );
}

/** Compact single-row stacked bar. Each segment is hover/focus-revealable and
 * shows its stage, duration and share in a tooltip — no verbose list needed. */
function StackedShareBar({ segments }: { segments: ShareSegment[] }) {
  const total = segments.reduce((sum, seg) => sum + seg.ms, 0);
  if (total <= 0) return null;
  return (
    <div className="flex h-3 w-full gap-px overflow-hidden rounded-full bg-muted ring-1 ring-inset ring-border/50">
      {segments.map((seg) => {
        const pct = (seg.ms / total) * 100;
        if (pct <= 0) return null;
        return (
          <Tooltip key={seg.key}>
            <TooltipTrigger asChild>
              <div
                tabIndex={0}
                className="h-full cursor-default outline-none transition-[filter] duration-150 first:rounded-l-full last:rounded-r-full hover:brightness-110 focus-visible:brightness-125"
                style={{ width: `${pct}%`, backgroundColor: seg.color }}
              />
            </TooltipTrigger>
            <StageTooltipContent segment={seg} share={Math.round(pct)} />
          </Tooltip>
        );
      })}
    </div>
  );
}

/** Wrapping legend of color-dot + stage + duration chips, each hover-revealable.
 * Pairs with `StackedShareBar` to give a glanceable summary without bulky rows. */
function StageLegend({ segments }: { segments: ShareSegment[] }) {
  const total = segments.reduce((sum, seg) => sum + seg.ms, 0);
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-1.5">
      {segments.map((seg) => (
        <Tooltip key={seg.key}>
          <TooltipTrigger asChild>
            <div
              tabIndex={0}
              className="flex cursor-default items-center gap-1.5 rounded outline-none transition-opacity hover:opacity-80 focus-visible:ring-1 focus-visible:ring-ring"
            >
              <span className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: seg.color }} />
              <code className="font-mono text-[11px] font-semibold text-foreground">{seg.key}</code>
              <span className="font-mono text-[10px] tabular-nums text-muted-foreground">
                {formatDuration(seg.ms)}
              </span>
              {seg.calls && seg.calls > 1 ? (
                <span className="rounded bg-primary/10 px-1 font-mono text-[9px] font-bold tabular-nums text-primary">
                  x{seg.calls}
                </span>
              ) : null}
            </div>
          </TooltipTrigger>
          <StageTooltipContent segment={seg} share={shareOf(seg.ms, total)} />
        </Tooltip>
      ))}
    </div>
  );
}

/** Stage breakdown: an interactive stacked bar over a compact wrapping legend.
 * Both surfaces share the same hover tooltips, so the panel stays one or two
 * rows tall instead of a column of bordered per-stage cards. */
function StagePanel({ segments }: { segments: ShareSegment[] }) {
  if (segments.length === 0) return null;
  return (
    <div className="flex flex-col gap-2.5">
      <StackedShareBar segments={segments} />
      <StageLegend segments={segments} />
    </div>
  );
}

/** A single proportional fill bar (one span's duration vs. the section max). */
function DurationBar({ ms, maxMs, color }: { ms: number; maxMs: number; color: string }) {
  const pct = maxMs > 0 ? Math.max(2, Math.min(100, (ms / maxMs) * 100)) : 0;
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div className="h-full rounded-full" style={{ width: `${pct}%`, backgroundColor: color }} />
    </div>
  );
}

type ToolCallDebug =
  NonNullable<LlmContextDebugResponse['iterations']>[number]['responseToolCalls'][number];

/** A tool call inside a model iteration. When it spawned a child execution
 * (e.g. grounded_answer), it can be expanded to drill into that child's own
 * stage breakdown for full execution transparency. */
function ToolCallRow({
  t,
  toolCall,
  maxIterationSpanMs,
  childExecutionId,
}: {
  t: TFunction;
  toolCall: ToolCallDebug;
  maxIterationSpanMs: number;
  childExecutionId?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const canDrill = Boolean(childExecutionId);
  const toolWaitTooltip = toolCall.durationMs != null && toolCall.durationMs > 0
    ? t('assistant.tooltipToolCall', {
        name: toolCall.name,
        wait: formatDuration(toolCall.durationMs),
      })
    : undefined;
  return (
    <article
      className={`rounded-md border px-2 py-1.5 text-[11px] ${
        toolCall.isError
          ? 'border-status-failed/30 bg-status-failed/5'
          : 'border-border/70 bg-background/60'
      }`}
    >
      <header className="flex items-center gap-2">
        {canDrill ? (
          <button
            type="button"
            onClick={() => setExpanded((value) => !value)}
            aria-expanded={expanded}
            aria-label={t('assistant.mcpChildExecution')}
            className="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
          >
            {expanded ? (
              <ChevronDown className="h-3.5 w-3.5" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5" />
            )}
          </button>
        ) : toolCall.isError ? (
          <AlertCircle className="h-3 w-3 shrink-0 text-status-failed" />
        ) : (
          <Wrench className="h-3 w-3 shrink-0 text-primary" />
        )}
        <Tooltip>
          <TooltipTrigger asChild>
            <code className="cursor-default truncate font-mono text-[11px] font-bold">
              {toolCall.name}
            </code>
          </TooltipTrigger>
          {toolWaitTooltip && <TooltipContent>{toolWaitTooltip}</TooltipContent>}
        </Tooltip>
        {toolCall.durationMs != null && toolCall.durationMs > 0 && (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                tabIndex={0}
                className="ml-auto shrink-0 cursor-default font-mono text-[10px] tabular-nums text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring rounded"
              >
                {t('assistant.mcpToolWait')} {formatDuration(toolCall.durationMs)}
              </span>
            </TooltipTrigger>
            <TooltipContent>{t('assistant.stageTipToolWait')}</TooltipContent>
          </Tooltip>
        )}
      </header>
      {toolCall.durationMs != null && toolCall.durationMs > 0 && (
        <div className="mt-1.5">
          <DurationBar ms={toolCall.durationMs} maxMs={maxIterationSpanMs} color={spanColor(2)} />
        </div>
      )}
      {canDrill && expanded && childExecutionId && (
        <ChildExecutionDrilldown t={t} executionId={childExecutionId} />
      )}
      {toolCall.argumentsJson && toolCall.argumentsJson !== '{}' && (
        <JsonDetails label={t('assistant.mcpToolsArgs')} value={toolCall.argumentsJson} />
      )}
      {toolCall.resultText && (
        <JsonDetails label={t('assistant.mcpToolsResult')} value={toolCall.resultText} />
      )}
      {hasJsonPayload(toolCall.resultJson) && (
        <JsonDetails label={t('assistant.mcpToolsPayload')} value={stringifyJson(toolCall.resultJson)} />
      )}
    </article>
  );
}

/** Lazily fetches a child execution and renders its pipeline-stage breakdown
 * (where the tool spent its time internally). Fetch only fires on expand. */
function ChildExecutionDrilldown({ t, executionId }: { t: TFunction; executionId: string }) {
  const { data, isLoading, isError } = useQuery({
    queryKey: ['assistant', 'child-execution', executionId],
    queryFn: () => queryApi.getExecution(executionId),
    staleTime: 60_000,
  });
  const { data: childContext } = useQuery({
    queryKey: ['assistant', 'child-llm-context', executionId],
    queryFn: () => queryApi.getExecutionLlmContext(executionId),
    staleTime: 60_000,
  });
  if (isLoading) {
    return (
      <div className="mt-2 flex items-center gap-2 rounded-md border border-border/60 bg-background/40 px-2 py-2 text-[10px] text-muted-foreground">
        <Loader2 className="h-3 w-3 animate-spin text-primary/70" />
        {t('assistant.debugInspectorLoading')}
      </div>
    );
  }
  if (isError || !data) {
    return (
      <div className="mt-2 rounded-md border border-status-failed/30 bg-status-failed/5 px-2 py-2 text-[10px] text-status-failed">
        {t('assistant.llmContextUnavailable')}
      </div>
    );
  }
  const stages = (data.runtimeStageSummaries ?? []).map((stage) => ({
    stage: stage.stageKind,
    durationMs: stage.durationMs ?? 0,
  }));
  return (
    <div className="mt-2 border-l-2 border-primary/40 bg-card/60 ml-1 pl-3 py-2 rounded-r-md">
      <div className="mb-1.5 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
        <Layers className="h-3 w-3" />
        {t('assistant.mcpChildExecution')}
      </div>
      {stages.length > 0 ? (
        <StagePanel
          segments={stages.map((stage, index) => ({
            key: stage.stage,
            ms: stage.durationMs,
            color: spanColor(index),
            tip: stageTooltip(t, stage.stage),
          }))}
        />
      ) : (
        <div className="text-[10px] text-muted-foreground">{t('assistant.mcpNoStageTelemetry')}</div>
      )}
      {childContext?.spans && childContext.spans.length > 0 && (
        <div className="mt-2">
          <SpansSection t={t} spans={childContext.spans} />
        </div>
      )}
    </div>
  );
}

type TurnSpanView = NonNullable<LlmContextDebugResponse['spans']>[number];

function spanKindColor(kind: string): string {
  switch (kind) {
    case 'db':
      return spanColor(1);
    case 'lane':
      return spanColor(2);
    case 'llm':
      return spanColor(5);
    case 'stage':
      return spanColor(0);
    default:
      return spanColor(6);
  }
}

/** Canonical kind order so groups render predictably (high-level lanes first,
 * then the raw DB calls they contain, then model calls and stage rollups). */
const SPAN_KIND_ORDER = ['lane', 'db', 'llm', 'stage'] as const;
/** Per-group cap: a heavy turn can fire dozens of DB spans; show the slowest
 * few and summarise the rest so the panel stays scannable. */
const SPANS_PER_GROUP = 10;

function spanGroupLabel(t: TFunction, kind: string): string {
  switch (kind) {
    case 'lane':
      return t('assistant.mcpSpanGroupLane');
    case 'db':
      return t('assistant.mcpSpanGroupDb');
    case 'llm':
      return t('assistant.mcpSpanGroupLlm');
    case 'stage':
      return t('assistant.mcpSpanGroupStage');
    default:
      return t('assistant.mcpSpanGroupOther');
  }
}

/** Heavy-operations view: recorded sub-operations grouped by kind (retrieval
 * lanes vs the DB calls they contain vs model calls), each sorted by duration
 * so the slowest section in every category is obvious and the lane aggregates
 * are not confused with their child DB queries. */
function SpansSection({ t, spans }: { t: TFunction; spans: TurnSpanView[] }) {
  if (spans.length === 0) return null;
  // One shared scale so bar lengths are comparable across groups.
  const maxMs = Math.max(1, ...spans.map((span) => span.durationMs));
  const presentKinds = Array.from(new Set(spans.map((span) => span.kind)));
  const orderedKinds = [
    ...SPAN_KIND_ORDER.filter((kind) => presentKinds.includes(kind)),
    ...presentKinds.filter(
      (kind) => !SPAN_KIND_ORDER.includes(kind as (typeof SPAN_KIND_ORDER)[number]),
    ),
  ];
  return (
    <div className="rounded-md border border-border/70 bg-card p-3">
      <div className="section-label mb-2">{t('assistant.mcpSpansTitle')}</div>
      <div className="flex flex-col gap-3">
        {orderedKinds.map((kind) => {
          const groupSpans = spans
            .filter((span) => span.kind === kind)
            .sort((left, right) => right.durationMs - left.durationMs);
          const shown = groupSpans.slice(0, SPANS_PER_GROUP);
          const hidden = groupSpans.length - shown.length;
          return (
            <div key={kind} className="flex flex-col gap-1.5">
              <div className="flex items-center gap-2">
                <span
                  className="h-2 w-2 shrink-0 rounded-full"
                  style={{ backgroundColor: spanKindColor(kind) }}
                />
                <span className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
                  {spanGroupLabel(t, kind)}
                </span>
                <span className="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
                  {groupSpans.length}
                </span>
              </div>
              {shown.map((span, index) => {
                const spanTooltipText = t('assistant.tooltipSpanDetail', {
                  name: span.detail ?? span.name,
                  duration: formatDuration(span.durationMs),
                  rows: span.rows ?? '-',
                });
                return (
                  <div key={`${span.name}-${span.startedOffsetMs}-${index}`}>
                    <div className="flex items-center gap-2">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <code className="cursor-default truncate font-mono text-[11px] font-semibold">
                            {span.name}
                          </code>
                        </TooltipTrigger>
                        <TooltipContent>{spanTooltipText}</TooltipContent>
                      </Tooltip>
                      {span.rows != null && (
                        <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">
                          {t('assistant.mcpSpansRows', { count: span.rows })}
                        </span>
                      )}
                      <span className="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
                        {formatDuration(span.durationMs)}
                      </span>
                    </div>
                    <div className="mt-1">
                      <DurationBar ms={span.durationMs} maxMs={maxMs} color={spanKindColor(kind)} />
                    </div>
                  </div>
                );
              })}
              {hidden > 0 && (
                <div className="text-[10px] text-muted-foreground">
                  {t('assistant.mcpSpanGroupMore', { count: hidden })}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number | string }) {
  return (
    <div className="flex flex-col gap-0.5 rounded-md border border-border/60 bg-card px-2 py-1.5">
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="font-mono text-sm font-semibold tabular-nums">{value}</span>
    </div>
  );
}


function InspectorTab({
  active,
  icon,
  label,
  count,
  onClick,
}: {
  active: boolean;
  icon: ReactNode;
  label: string;
  count: number;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={`flex min-w-0 items-center justify-start gap-1.5 rounded-md px-2 py-1.5 text-[11px] font-semibold transition-colors ${
        active
          ? 'bg-card text-foreground shadow-sm ring-1 ring-border/70'
          : 'text-muted-foreground hover:bg-card/70 hover:text-foreground'
      }`}
    >
      <span className="shrink-0">{icon}</span>
      <span className="truncate">{label}</span>
      <span className="shrink-0 rounded bg-muted px-1 font-mono text-[10px] tabular-nums text-muted-foreground">
        {count}
      </span>
    </button>
  );
}

function ContextTranscriptCard({
  t,
  entry,
}: {
  t: TFunction;
  entry: ContextTranscriptEntry;
}) {
  const contentHeightClass = entry.role === 'system' ? 'max-h-32' : 'max-h-60';
  return (
    <article
      className={`rounded-md border p-2 ${
        entry.isError
          ? 'border-status-failed/30 bg-status-failed/5'
          : 'border-border/60 bg-background/60'
      }`}
    >
      <header className="mb-1 flex min-w-0 items-center gap-2">
        <span
          className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${
            entry.role === 'tool'
              ? entry.isError
                ? 'bg-status-failed/10 text-status-failed'
                : 'bg-status-ready/10 text-status-ready'
              : 'bg-primary/10 text-primary'
          }`}
        >
          {contextEntryLabel(t, entry)}
        </span>
        {entry.toolName && (
          <Tooltip>
            <TooltipTrigger asChild>
              <code className="truncate font-mono text-[10px] text-muted-foreground">
                {entry.toolName}
              </code>
            </TooltipTrigger>
            <TooltipContent>{entry.toolName}</TooltipContent>
          </Tooltip>
        )}
        {entry.toolCallId && (
          <span className="ml-auto truncate font-mono text-[10px] text-muted-foreground">
            {t('assistant.toolCallIdLabel')}: {entry.toolCallId}
          </span>
        )}
      </header>
      {entry.content && (
        <pre className={`${contentHeightClass} overflow-auto whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-foreground/80`}>
          {entry.content}
        </pre>
      )}
      {entry.toolCalls && entry.toolCalls.length > 0 && (
        <div className="mt-2 space-y-1">
          {entry.toolCalls.map((toolCall, index) => (
            <div
              key={`${toolCall.id || toolCall.name}-${index}`}
              className={`rounded border p-2 font-mono text-[10px] ${
                toolCall.isError
                  ? 'border-status-failed/30 bg-status-failed/5 text-status-failed'
                  : 'border-status-warning/30 bg-status-warning/5 text-status-warning'
              }`}
            >
              <div className="mb-1 flex min-w-0 items-center gap-2">
                <Wrench className="h-3 w-3 shrink-0" />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <code className="truncate font-bold">{toolCall.name}</code>
                  </TooltipTrigger>
                  <TooltipContent>{toolCall.name}</TooltipContent>
                </Tooltip>
                <span className="ml-auto truncate text-[9px] opacity-75">
                  {toolCall.id}
                </span>
              </div>
              {toolCall.argumentsJson && toolCall.argumentsJson !== '{}' && (
                <pre className="max-h-36 overflow-auto whitespace-pre-wrap [overflow-wrap:anywhere]">
                  {formatJsonish(toolCall.argumentsJson)}
                </pre>
              )}
            </div>
          ))}
        </div>
      )}
      {hasJsonPayload(entry.resultJson) && (
        <JsonDetails
          label={t('assistant.mcpToolsPayload')}
          value={stringifyJson(entry.resultJson)}
        />
      )}
    </article>
  );
}

function contextEntryLabel(t: TFunction, entry: ContextTranscriptEntry) {
  if (entry.phase === 'final') return t('assistant.contextPhaseFinal');
  switch (entry.role) {
    case 'system':
      return t('assistant.ctxRoleSystem');
    case 'user':
      return t('assistant.ctxRoleUser');
    case 'tool':
      return t('assistant.ctxRoleTool');
    case 'assistant':
      return t('assistant.ctxRoleModel');
    default:
      return entry.role;
  }
}

function JsonDetails({
  label,
  value,
  defaultOpen = false,
}: {
  label: string;
  value: string;
  defaultOpen?: boolean;
}) {
  const identity = useMemo(
    () => `${label}:${defaultOpen ? 'open' : 'closed'}:${jsonDetailsFingerprint(value)}`,
    [defaultOpen, label, value],
  );
  return (
    <JsonDetailsContent
      key={identity}
      label={label}
      value={value}
      defaultOpen={defaultOpen}
    />
  );
}

function JsonDetailsContent({
  label,
  value,
  defaultOpen,
}: {
  label: string;
  value: string;
  defaultOpen: boolean;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);
  const formatted = formatJsonish(value);
  return (
    <details
      className="mt-1.5"
      open={isOpen}
      onToggle={(event) => setIsOpen(event.currentTarget.open)}
    >
      <summary className="cursor-pointer text-[10px] font-semibold uppercase tracking-wide text-muted-foreground hover:text-foreground">
        {label}
      </summary>
      <pre className="mt-1 max-h-40 overflow-auto rounded border border-border/40 bg-background p-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
        {formatted}
      </pre>
    </details>
  );
}

function jsonDetailsFingerprint(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${value.length}:${hash >>> 0}`;
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
