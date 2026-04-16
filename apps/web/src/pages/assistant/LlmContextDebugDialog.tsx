import { memo } from 'react';
import { useTranslation } from 'react-i18next';
import { X as IconX } from 'lucide-react';
import type { LlmContextDebugResponse } from '@/api/query';

type LlmContextDebugDialogProps = {
  snapshot: LlmContextDebugResponse;
  onClose: () => void;
};

function LlmContextDebugDialogImpl({ snapshot, onClose }: LlmContextDebugDialogProps) {
  const { t } = useTranslation();
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/60 backdrop-blur-sm p-4">
      <div className="bg-card border rounded-xl shadow-elevated w-full max-w-5xl max-h-[90vh] flex flex-col">
        <div className="flex items-start justify-between gap-4 px-5 py-4 border-b">
          <div className="min-w-0">
            <div className="text-sm font-semibold">{t('assistant.llmContextTitle')}</div>
            <div className="text-xs text-muted-foreground mt-0.5 truncate">
              execution {snapshot.executionId} · {snapshot.totalIterations}{' '}
              {t('assistant.iterations')}
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="text-muted-foreground hover:text-foreground transition-colors"
            aria-label={t('assistant.close')}
          >
            <IconX className="h-4 w-4" />
          </button>
        </div>
        <div className="flex-1 overflow-auto px-5 py-4 space-y-6">
          {snapshot.iterations.map((iter) => (
            <div key={iter.iteration} className="space-y-2">
              <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {t('assistant.iteration')} #{iter.iteration} · {iter.providerKind}/
                {iter.modelName}
              </div>
              <details className="border rounded-md">
                <summary className="cursor-pointer px-3 py-2 text-xs font-medium bg-muted/40 rounded-md">
                  {t('assistant.requestMessages')} ({iter.requestMessages.length})
                </summary>
                <div className="p-3 space-y-2 font-mono text-[11px] leading-relaxed">
                  {iter.requestMessages.map((m, i) => (
                    <div key={i} className="border-l-2 border-primary/30 pl-2">
                      <div className="text-primary font-semibold mb-0.5">[{m.role}]</div>
                      {m.content && (
                        <pre className="whitespace-pre-wrap break-words text-foreground/80 max-h-60 overflow-auto">
                          {m.content}
                        </pre>
                      )}
                      {m.toolCalls && m.toolCalls.length > 0 && (
                        <div className="mt-1 text-status-warning">
                          {m.toolCalls.map((tc) => (
                            <div key={tc.id}>
                              → {tc.name}({tc.argumentsJson})
                            </div>
                          ))}
                        </div>
                      )}
                      {m.toolCallId && (
                        <div className="text-muted-foreground mt-0.5">
                          tool_call_id: {m.toolCallId}
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              </details>
              {iter.responseText && (
                <details className="border rounded-md">
                  <summary className="cursor-pointer px-3 py-2 text-xs font-medium bg-muted/40 rounded-md">
                    {t('assistant.responseText')}
                  </summary>
                  <pre className="p-3 font-mono text-[11px] whitespace-pre-wrap break-words max-h-80 overflow-auto">
                    {iter.responseText}
                  </pre>
                </details>
              )}
              {iter.responseToolCalls.length > 0 && (
                <details className="border rounded-md">
                  <summary className="cursor-pointer px-3 py-2 text-xs font-medium bg-muted/40 rounded-md">
                    {t('assistant.responseToolCalls')} ({iter.responseToolCalls.length})
                  </summary>
                  <div className="p-3 space-y-3 font-mono text-[11px]">
                    {iter.responseToolCalls.map((tc) => (
                      <div
                        key={tc.id}
                        className="border-l-2 border-status-warning/40 pl-2"
                      >
                        <div
                          className={
                            tc.isError
                              ? 'text-status-failed font-semibold'
                              : 'text-status-warning font-semibold'
                          }
                        >
                          {tc.name}({tc.argumentsJson})
                        </div>
                        {tc.resultText && (
                          <pre className="whitespace-pre-wrap break-words text-foreground/70 max-h-60 overflow-auto mt-1">
                            {tc.resultText}
                          </pre>
                        )}
                      </div>
                    ))}
                  </div>
                </details>
              )}
            </div>
          ))}
          {snapshot.finalAnswer && (
            <div className="space-y-1">
              <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {t('assistant.finalAnswer')}
              </div>
              <pre className="border rounded-md p-3 font-mono text-[11px] whitespace-pre-wrap break-words max-h-80 overflow-auto">
                {snapshot.finalAnswer}
              </pre>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export const LlmContextDebugDialog = memo(LlmContextDebugDialogImpl);
