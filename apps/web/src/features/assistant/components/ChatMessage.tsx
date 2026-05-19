import { memo, useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import { BrainCircuit, CheckCircle2, Loader2, Wrench } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import type { AssistantAgentActivityEvent, AssistantMessage } from '@/shared/types';
import { VERIFICATION_CONFIG, verificationLabel } from "../model/verificationConfig";

type ChatMessageProps = {
  t: TFunction;
  message: AssistantMessage;
};

function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
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

function ChatMessageImpl({ t, message }: ChatMessageProps) {
  const isUser = message.role === 'user';
  const vcState = message.evidence?.verificationState;
  const vc = vcState && vcState !== 'not_run' ? VERIFICATION_CONFIG[vcState] : null;
  const vcLabel = vcState && vcState !== 'not_run' ? verificationLabel(vcState, t) : null;
  const isPendingAssistant = !isUser && !message.content;
  const messageWidthClass = isUser
    ? 'max-w-[80%]'
    : isPendingAssistant
      ? 'w-full max-w-[560px]'
      : 'max-w-[80%]';

  return (
    <div className={`flex ${isUser ? 'justify-end' : 'justify-start'} animate-fade-in`}>
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
        {vc && (
          <div className="flex items-center gap-2 text-xs">
            <vc.icon className={`h-3 w-3 ${vc.cls}`} />
            <span className={`font-semibold ${vc.cls}`}>{vcLabel}</span>
          </div>
        )}
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
