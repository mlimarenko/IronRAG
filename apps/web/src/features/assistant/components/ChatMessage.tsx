import { memo, useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import {
  BrainCircuit,
  Bug,
  Check,
  CheckCircle2,
  Copy,
  ExternalLink,
  FileText,
  Layers,
  Loader2,
  Wrench,
} from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import type { AssistantAgentActivityEvent, AssistantMessage } from '@/shared/types';
import { VerificationChip } from './VerificationChip';

type ChatMessageProps = {
  t: TFunction;
  message: AssistantMessage;
  responseMs?: number;
  /** Total distinct evidence sources (segments + entities) backing this answer. */
  totalSourceCount?: number;
  /** Opens the evidence/citations panel scoped to this message. */
  onOpenEvidence?: () => void;
  /** Opens the debug inspector for this turn (developer mode only). */
  onInspect?: () => void;
  /** When true, surfaces the per-message debug affordance. */
  developerMode?: boolean;
};

type AnswerSourceLink = {
  href: string;
  label: string;
  kind: 'stored_document' | 'external_url';
};

const MAX_INLINE_SOURCE_LINKS = 5;

function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

function formatTimestamp(isoString: string): string {
  const date = new Date(isoString);
  if (Number.isNaN(date.getTime())) return '';
  const today = new Date();
  const isToday =
    date.getFullYear() === today.getFullYear() &&
    date.getMonth() === today.getMonth() &&
    date.getDate() === today.getDate();
  const timePart = date.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });
  if (isToday) return timePart;
  const datePart = date.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
  });
  return `${datePart} ${timePart}`;
}

function formatLatency(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function eventLabel(event: AssistantAgentActivityEvent, t: TFunction): string {
  switch (event.type) {
    case 'started':
      return t('assistant.activity.started');
    case 'model_request':
      return t('assistant.activity.modelRequest', {
        model: event.model_name ?? t('assistant.activity.modelUnknown'),
        iteration: event.iteration ?? 1,
      });
    case 'model_response':
      return event.has_final_answer
        ? t('assistant.activity.modelFinal')
        : t('assistant.activity.modelToolPlan', {
            count: event.tool_call_count ?? 0,
          });
    case 'tool_call_started':
      return t('assistant.activity.toolStarted', {
        tool: event.tool_name ?? t('assistant.activity.toolUnknown'),
      });
    case 'tool_call_finished':
      return event.is_error
        ? t('assistant.activity.toolFailed', {
            tool: event.tool_name ?? t('assistant.activity.toolUnknown'),
          })
        : t('assistant.activity.toolFinished', {
            tool: event.tool_name ?? t('assistant.activity.toolUnknown'),
          });
    case 'working':
      return t('assistant.activity.working');
    case 'persisting':
      return t('assistant.activity.persisting');
    default:
      return t('assistant.activity.working');
  }
}

function activityHeadline(event: AssistantAgentActivityEvent | undefined, t: TFunction): string {
  if (event?.type === 'tool_call_started') {
    return t('assistant.activity.toolRunningTitle');
  }
  return eventLabel(event ?? { type: 'started' }, t);
}

function activityStatus(
  event: AssistantAgentActivityEvent | undefined,
  live: boolean,
  t: TFunction,
): string {
  if (
    event?.type === 'tool_call_started' &&
    event.tool_name
  ) {
    return event.tool_name;
  }
  return live ? t('assistant.activity.working') : t('assistant.activity.complete');
}

function renderActivityIcon(event: AssistantAgentActivityEvent | undefined) {
  const className = `h-4 w-4 ${
    event?.type === 'persisting' ? 'text-emerald-600' : 'text-primary'
  }`;
  if (event?.type?.startsWith('tool_call')) return <Wrench className={className} />;
  if (event?.type === 'model_request' || event?.type === 'model_response') {
    return <BrainCircuit className={className} />;
  }
  if (event?.type === 'persisting') return <CheckCircle2 className={className} />;
  return <Loader2 className={className} />;
}

function collectAnswerSourceLinks(message: AssistantMessage): AnswerSourceLink[] {
  const seen = new Set<string>();
  const links: AnswerSourceLink[] = [];

  for (const ref of message.evidence?.segmentRefs ?? []) {
    const sourceAccess = ref.sourceAccess;
    const fallbackSourceUri = ref.sourceUri?.trim();
    const href =
      sourceAccess?.href ??
      (fallbackSourceUri?.startsWith('http://') || fallbackSourceUri?.startsWith('https://')
        ? fallbackSourceUri
        : null);
    if (!href || seen.has(href)) continue;

    const label = (ref.documentTitle || ref.documentName || href).trim();
    links.push({
      href,
      label: label || href,
      kind: sourceAccess?.kind ?? 'external_url',
    });
    seen.add(href);
    if (links.length >= MAX_INLINE_SOURCE_LINKS) break;
  }

  return links;
}

function PendingAssistantActivity({
  events = [],
  live = true,
  startedAt,
  t,
}: {
  events?: AssistantAgentActivityEvent[];
  live?: boolean;
  startedAt: string;
  t: TFunction;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return undefined;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [live]);

  const startedAtMs = Date.parse(startedAt);
  const elapsed = Number.isFinite(startedAtMs) ? now - startedAtMs : 0;
  const latest = events.at(-1);
  const latestLabel = activityHeadline(latest, t);
  const statusLabel = activityStatus(latest, live, t);

  return (
    <div
      className={`agent-activity-card w-full max-w-[560px] overflow-hidden rounded-xl border border-primary/15 bg-card text-xs shadow-lifted ${
        live ? 'agent-activity-card-live' : ''
      }`}
    >
      <div className="flex items-start gap-3 p-3.5">
        <div className="relative mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-primary/20 bg-primary/10">
          <span className="relative z-10">{renderActivityIcon(latest)}</span>
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-start gap-3">
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-semibold tracking-tight text-foreground">
                {latestLabel}
              </div>
              <div className="mt-1 flex items-center gap-1.5 text-[11px] text-muted-foreground">
                <span
                  className={`h-1.5 w-1.5 rounded-full ${
                    live ? 'bg-primary' : 'bg-status-ready'
                  }`}
                />
                <span className="truncate">{statusLabel}</span>
              </div>
            </div>
            <span className="shrink-0 rounded-md border border-border/70 bg-background/70 px-2 py-1 font-mono text-[11px] tabular-nums text-muted-foreground">
              {formatElapsed(elapsed)}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}

const markdownComponents = {
  a: ({
    children,
    className,
    href,
    node: _node,
    ...props
  }: React.AnchorHTMLAttributes<HTMLAnchorElement> & { node?: unknown }) => (
    <a
      {...props}
      href={href}
      target={href ? '_blank' : undefined}
      rel="noopener noreferrer"
      className={[
        'break-words font-semibold text-primary underline decoration-primary/40 underline-offset-2 transition-colors',
        'hover:decoration-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
        className,
      ].filter(Boolean).join(' ')}
    >
      {children}
    </a>
  ),
  code: ({ className, children, ...props }: React.HTMLAttributes<HTMLElement>) => {
    const isInline = !className;
    return isInline ? (
      <code className="bg-muted px-1 py-0.5 rounded text-xs" {...props}>
        {children}
      </code>
    ) : (
      <pre className="bg-muted rounded-md p-3 overflow-x-auto text-xs">
        <code className={className} {...props}>
          {children}
        </code>
      </pre>
    );
  },
  table: ({ children }: { children?: React.ReactNode }) => (
    <div className="overflow-x-auto">
      <table className="min-w-full text-xs border-collapse">{children}</table>
    </div>
  ),
  th: ({ children }: { children?: React.ReactNode }) => (
    <th className="border border-border px-2 py-1 bg-muted font-medium text-left">
      {children}
    </th>
  ),
  td: ({ children }: { children?: React.ReactNode }) => (
    <td className="border border-border px-2 py-1">{children}</td>
  ),
};

function CopyAnswerButton({ t, content }: { t: TFunction; content: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return undefined;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const handleCopy = async () => {
    try {
      await navigator.clipboard?.writeText(content);
      setCopied(true);
    } catch {
      // Clipboard may be blocked (insecure context / denied permission); the
      // action is best-effort and must never throw into the render tree.
    }
  };

  return (
    <button
      type="button"
      onClick={handleCopy}
      aria-label={copied ? t('assistant.copied') : t('assistant.copyAnswer')}
      title={copied ? t('assistant.copied') : t('assistant.copyAnswer')}
      className="inline-flex items-center gap-1 rounded-md border border-border/50 bg-background/60 px-1.5 py-0.5 text-[10px] font-semibold text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1"
    >
      {copied ? (
        <Check className="h-3 w-3 text-status-ready" aria-hidden="true" />
      ) : (
        <Copy className="h-3 w-3" aria-hidden="true" />
      )}
      <span>{copied ? t('assistant.copied') : t('assistant.copyAnswer')}</span>
    </button>
  );
}

function ChatMessageImpl({
  t,
  message,
  responseMs,
  totalSourceCount,
  onOpenEvidence,
  onInspect,
  developerMode,
}: ChatMessageProps) {
  const isUser = message.role === 'user';
  const vcState = message.evidence?.verificationState;
  const showVerdict = !isUser && vcState != null && vcState !== 'not_run';
  const isPendingAssistant = !isUser && !message.content;
  const sourceLinks = !isUser && !isPendingAssistant ? collectAnswerSourceLinks(message) : [];
  const hiddenSourceCount =
    totalSourceCount != null ? Math.max(0, totalSourceCount - sourceLinks.length) : 0;
  const hasEvidence = Boolean(
    message.evidence &&
      (message.evidence.segmentRefs.length > 0 ||
        message.evidence.entityRefs.length > 0 ||
        message.evidence.factRefs.length > 0),
  );
  const showFooterActions =
    !isUser && !isPendingAssistant && Boolean(message.content);
  const messageWidthClass = isUser
    ? 'max-w-[80%]'
    : isPendingAssistant
      ? 'w-full max-w-[560px]'
      : 'max-w-[80%]';

  const timestampFormatted = message.timestamp ? formatTimestamp(message.timestamp) : '';
  const showTimestamp = Boolean(timestampFormatted) && !isPendingAssistant;
  const showLatency = !isUser && !isPendingAssistant && responseMs != null && responseMs > 0;

  return (
    <div className={`flex flex-col gap-0.5 ${isUser ? 'items-end' : 'items-start'} animate-fade-in`}>
      <div
        className={`${messageWidthClass} ${
          isUser ? 'text-primary-foreground rounded-2xl rounded-br-sm px-4 py-3' : 'space-y-2'
        }`}
        style={
          isUser
            ? {
                background:
                  'linear-gradient(135deg, hsl(var(--primary)), hsl(224 76% 42%))',
                boxShadow: '0 2px 8px -2px hsl(var(--primary) / 0.4)',
              }
            : undefined
        }
      >
        <div
          className={`text-sm leading-relaxed ${
            !isUser && !isPendingAssistant
              ? 'bg-card border rounded-2xl rounded-bl-sm px-4 py-3 shadow-soft'
              : ''
          }`}
        >
          {!isUser && !message.content && (
            <PendingAssistantActivity
              events={message.activityEvents}
              startedAt={message.timestamp}
              t={t}
            />
          )}
          {!isUser ? (
            <div className="prose prose-sm dark:prose-invert max-w-none">
              <ReactMarkdown components={markdownComponents}>
                {message.content}
              </ReactMarkdown>
              {sourceLinks.length > 0 && (
                <div className="not-prose mt-3 rounded-lg border border-dashed border-primary/25 bg-primary/[0.03] px-3 py-2.5">
                  <div className="mb-2 text-[11px] font-semibold uppercase text-muted-foreground">
                    {t('assistant.sources')}
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {sourceLinks.map((link) => (
                      <a
                        key={link.href}
                        href={link.href}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex max-w-full items-center gap-1 rounded-md border border-primary/30 bg-primary/5 px-2 py-1 text-xs font-semibold text-primary underline decoration-primary/50 underline-offset-2 transition-colors hover:bg-primary/10 hover:decoration-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                        title={link.label}
                      >
                        {link.kind === 'stored_document' ? (
                          <FileText className="h-3 w-3 shrink-0" />
                        ) : (
                          <ExternalLink className="h-3 w-3 shrink-0" />
                        )}
                        <span className="min-w-0 truncate">{link.label}</span>
                      </a>
                    ))}
                    {onOpenEvidence && (hiddenSourceCount > 0 || hasEvidence) && (
                      <button
                        type="button"
                        onClick={onOpenEvidence}
                        className="inline-flex items-center gap-1 rounded-md border border-primary/40 bg-primary/10 px-2 py-1 text-xs font-bold text-primary transition-colors hover:bg-primary/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                      >
                        <Layers className="h-3 w-3 shrink-0" aria-hidden="true" />
                        {hiddenSourceCount > 0
                          ? t('assistant.seeAllSources', { count: totalSourceCount ?? 0 })
                          : t('assistant.viewEvidence')}
                      </button>
                    )}
                  </div>
                </div>
              )}
            </div>
          ) : (
            message.content.split('\n').map((line, i) => (
              <p key={i} className={i > 0 ? 'mt-2' : ''}>
                {line}
              </p>
            ))
          )}
        </div>
      </div>
      {showVerdict && vcState && (
        <VerificationChip
          t={t}
          state={vcState}
          warnings={message.evidence?.verificationWarnings}
          className="mt-1.5"
        />
      )}
      {showFooterActions && (
        <div className="mt-1 flex flex-wrap items-center gap-1.5 px-1">
          <CopyAnswerButton t={t} content={message.content} />
          {developerMode && onInspect && (
            <button
              type="button"
              onClick={onInspect}
              aria-label={t('assistant.inspectTurn')}
              title={t('assistant.inspectTurn')}
              className="inline-flex items-center gap-1 rounded-md border border-border/50 bg-background/60 px-1.5 py-0.5 text-[10px] font-semibold text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1"
            >
              <Bug className="h-3 w-3" aria-hidden="true" />
              <span>{t('assistant.inspectTurn')}</span>
            </button>
          )}
        </div>
      )}
      {(showTimestamp || showLatency) && (
        <div className={`flex items-center gap-2 px-1 ${isUser ? 'justify-end' : 'justify-start'}`}>
          {showTimestamp && (
            <time
              dateTime={message.timestamp}
              className="font-mono text-[10px] tabular-nums text-muted-foreground/60"
            >
              {t('assistant.messageTimestamp', { time: timestampFormatted })}
            </time>
          )}
          {showLatency && responseMs != null && (
            <span className="rounded border border-border/50 bg-background/60 px-1.5 py-0.5 font-mono text-[10px] tabular-nums text-muted-foreground/70">
              {t('assistant.messageLatency', { duration: formatLatency(responseMs) })}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Memoized per-message renderer. During streaming the parent creates a new
 * messages array every chunk, but React.memo's shallow compare on the
 * individual `message` object reference means only the message that the
 * streaming delta actually touched re-renders (and re-runs ReactMarkdown).
 * Historical messages skip reconciliation entirely.
 */
export const ChatMessage = memo(ChatMessageImpl);
