import { memo } from 'react';
import type { TFunction } from 'i18next';
import { Bug, Loader2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import type { AssistantMessage } from '@/types';
import { VERIFICATION_CONFIG } from './verificationConfig';

type ChatMessageProps = {
  t: TFunction;
  message: AssistantMessage;
  onOpenDebug?: (executionId: string) => void;
};

const markdownComponents = {
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

function ChatMessageImpl({ t, message, onOpenDebug }: ChatMessageProps) {
  const isUser = message.role === 'user';
  const vcState = message.evidence?.verificationState;
  const vc = vcState && vcState !== 'not_run' ? VERIFICATION_CONFIG[vcState] : null;

  return (
    <div className={`flex ${isUser ? 'justify-end' : 'justify-start'} animate-fade-in`}>
      <div
        className={`max-w-[80%] ${
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
            <span className={`font-semibold ${vc.cls}`}>{t(vc.labelKey)}</span>
          </div>
        )}
        <div
          className={`text-sm leading-relaxed ${
            !isUser ? 'bg-card border rounded-2xl rounded-bl-sm px-4 py-3 shadow-soft' : ''
          }`}
        >
          {!isUser && message.executionId && onOpenDebug && (
            <button
              type="button"
              onClick={() => message.executionId && onOpenDebug(message.executionId)}
              className="float-right ml-2 -mt-1 text-muted-foreground/50 hover:text-muted-foreground transition-colors"
              title={t('assistant.showLlmContext')}
              aria-label={t('assistant.showLlmContext')}
            >
              <Bug className="h-3 w-3" />
            </button>
          )}
          {!isUser && !message.content && (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" />
              <span>{t('assistant.grounding')}</span>
            </div>
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
