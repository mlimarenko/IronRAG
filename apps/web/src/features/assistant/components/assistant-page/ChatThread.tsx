import { useEffect, useRef } from 'react';
import type { TFunction } from 'i18next';
import { Brain } from 'lucide-react';
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState';
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
  const lastMessage = messages[messages.length - 1];
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
        <div className="flex min-h-full flex-col items-center justify-center py-8 animate-fade-in">
          <WorkbenchEmptyState
            icon={<Brain className="h-7 w-7 text-primary" />}
            title={t('assistant.askQuestion')}
            description={t('assistant.askQuestionDesc')}
            action={
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5 max-w-md w-full">
                {STARTER_PROMPT_IDS.map((id) => {
                  const prompt = t(`assistant.starterPrompts.${id}`);
                  return (
                    <button
                      key={id}
                      className="rounded-lg border px-3 py-2.5 text-left text-sm font-medium transition-colors hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
                      onClick={() => onStarterPromptSelect(prompt)}
                    >
                      {prompt}
                    </button>
                  );
                })}
              </div>
            }
          />
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
