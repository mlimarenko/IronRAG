import { useEffect, useRef } from 'react';
import type { TFunction } from 'i18next';
import { Brain } from 'lucide-react';
import type { AssistantMessage } from '@/shared/types';
import { ChatMessage } from '../ChatMessage';
import { countDistinctSources, STARTER_PROMPT_IDS } from './assistantPageState';

type ChatThreadProps = {
  t: TFunction;
  messages: AssistantMessage[];
  developerMode?: boolean;
  onStarterPromptSelect: (prompt: string) => void;
  onOpenEvidence: (message: AssistantMessage) => void;
  onInspect: (executionId: string) => void;
};

export function ChatThread({
  t,
  messages,
  developerMode,
  onStarterPromptSelect,
  onOpenEvidence,
  onInspect,
}: ChatThreadProps) {
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const lastMessage = messages.at(-1);
  const scrollSignature = lastMessage
    ? [
        messages.length,
        lastMessage.id,
        lastMessage.content?.length ?? 0,
        lastMessage.activityEvents?.length ?? 0,
        lastMessage.executionId ?? '',
      ].join(':')
    : '';

  useEffect(() => {
    if (messages.length === 0) return;
    const frame = requestAnimationFrame(() => {
      if (typeof messagesEndRef.current?.scrollIntoView === 'function') {
        messagesEndRef.current.scrollIntoView({
          behavior: 'smooth',
          block: 'end',
        });
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [messages.length, scrollSignature]);

  return (
    <div className="flex-1 overflow-y-auto p-4 space-y-4">
      {messages.length === 0 ? (
        <div className="flex-1 flex flex-col items-center justify-center py-16 animate-fade-in">
          <div
            className="w-16 h-16 rounded-2xl flex items-center justify-center mb-5"
            style={{
              background:
                'linear-gradient(135deg, hsl(var(--primary) / 0.15), hsl(var(--primary) / 0.05))',
              boxShadow: '0 0 0 1px hsl(var(--primary) / 0.1)',
            }}
          >
            <Brain className="h-8 w-8 text-primary" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t('assistant.askQuestion')}
          </h2>
          <p className="text-sm text-muted-foreground mt-1.5 mb-6">
            {t('assistant.askQuestionDesc')}
          </p>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5 max-w-md w-full">
            {STARTER_PROMPT_IDS.map((id) => {
              const prompt = t(`assistant.starterPrompts.${id}`);
              return (
                <button
                  key={id}
                  className="text-left p-4 rounded-xl border hover:bg-accent/50 hover:shadow-soft transition-all duration-200 text-sm font-medium"
                  onClick={() => onStarterPromptSelect(prompt)}
                >
                  {prompt}
                </button>
              );
            })}
          </div>
        </div>
      ) : (
        messages.map((message, index) => {
          let responseMs: number | undefined;
          if (message.role === 'assistant') {
            if (typeof message.durationMs === 'number' && message.durationMs > 0) {
              // Server-authoritative wall-clock; immune to client↔server skew.
              responseMs = message.durationMs;
            } else if (message.timestamp) {
              // Reload path: both timestamps are server-stamped, so their
              // delta is a single-clock measurement.
              const assistantMs = Date.parse(message.timestamp);
              for (let i = index - 1; i >= 0; i -= 1) {
                const prev = messages[i];
                if (prev?.role === 'user' && prev.timestamp) {
                  const userMs = Date.parse(prev.timestamp);
                  const delta = assistantMs - userMs;
                  if (Number.isFinite(delta) && delta > 0) {
                    responseMs = delta;
                  }
                  break;
                }
              }
            }
          }
          const executionId = message.executionId ?? undefined;
          return (
            <ChatMessage
              key={message.id}
              t={t}
              message={message}
              responseMs={responseMs}
              developerMode={developerMode}
              totalSourceCount={
                message.role === 'assistant' ? countDistinctSources(message) : undefined
              }
              onOpenEvidence={
                message.role === 'assistant' && message.evidence
                  ? () => onOpenEvidence(message)
                  : undefined
              }
              onInspect={
                message.role === 'assistant' && executionId
                  ? () => onInspect(executionId)
                  : undefined
              }
            />
          );
        })
      )}

      <div ref={messagesEndRef} />
    </div>
  );
}
