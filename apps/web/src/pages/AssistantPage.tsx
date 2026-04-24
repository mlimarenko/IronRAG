import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { useNavigate } from 'react-router-dom';
import {
  mapAssistantMessage,
  mapAssistantSession,
  mapAssistantTurnToEvidence,
} from '@/adapters/assistant';
import { errorMessage } from '@/lib/errorMessage';
import { queryApi } from '@/api';
import type { AssistantTurnExecutionResponse } from '@/api/query';
import { SseTransportUnavailableError } from '@/api/query';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import {
  AlertTriangle,
  Brain,
  FileText,
  Loader2,
  MessageSquare,
  Send,
} from 'lucide-react';
import type { LlmContextDebugResponse } from '@/api/query';
import type { AssistantMessage, AssistantSession, EvidenceBundle } from '@/types';

import { SessionRail } from './assistant/SessionRail';
import { ChatMessage } from './assistant/ChatMessage';
import { EvidencePanel } from './assistant/EvidencePanel';
import { LlmContextDebugDialog } from './assistant/LlmContextDebugDialog';

const STARTER_PROMPT_KEYS = [
  'assistant.starterPrompts.technologies',
  'assistant.starterPrompts.deployment',
  'assistant.starterPrompts.security',
  'assistant.starterPrompts.storage',
] as const;

type TranslateFn = ReturnType<typeof useTranslation>['t'];

// Diagnose a failed turn and return a user-facing, localised message.
// Strings live in `i18n/*.json` under `assistant.errorDiagnosis.*` — do
// not inline translated text here. Default branch falls back to the raw
// error string so operators can still read the underlying failure.
function diagnoseTurnError(err: unknown, raw: string, t: TranslateFn): string {
  const lower = raw.toLowerCase();
  const looksLikeNetworkReject =
    lower.includes('networkerror') ||
    lower.includes('failed to fetch') ||
    lower.includes('load failed') ||
    lower.includes('input stream') ||
    lower.includes('stream ended');
  // If we get here after `createTurnWithFallback`, the original SSE
  // request was blocked AND the non-SSE fallback was also rejected —
  // both at the network layer, no backend involvement. Canonical cause
  // in practice is a browser-side block (Strict Tracking Protection,
  // uBlock / Privacy Badger, corporate TLS-inspection proxy, service
  // worker) OR a reverse-proxy that is buffering the SSE stream.
  if (err instanceof SseTransportUnavailableError || looksLikeNetworkReject) {
    return t('assistant.errorDiagnosis.networkBlocked');
  }
  if (lower.includes('timeout') || lower.includes('timed out')) {
    return t('assistant.errorDiagnosis.timeout');
  }
  if (lower.includes('401') || lower.includes('unauthorized')) {
    return t('assistant.errorDiagnosis.unauthorized');
  }
  return raw;
}

export default function AssistantPage() {
  const { t } = useTranslation();
  const { activeLibrary, activeWorkspace, locale } = useApp();
  const navigate = useNavigate();

  const [sessions, setSessions] = useState<AssistantSession[]>([]);
  const [activeSession, setActiveSession] = useState<string | null>(null);
  const [messages, setMessages] = useState<AssistantMessage[]>([]);
  const [inputText, setInputText] = useState('');
  const [isExecuting, setIsExecuting] = useState(false);
  const [retryable, setRetryable] = useState<{ question: string; diagnosis: string } | null>(null);
  const [sessionSearch, setSessionSearch] = useState('');
  const [showEvidence, setShowEvidence] = useState(true);
  const [showSessionRail, setShowSessionRail] = useState(true);
  const [debugContext, setDebugContext] = useState<LlmContextDebugResponse | null>(null);
  const [debugLoadingId, setDebugLoadingId] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const openDebugFor = useCallback(
    async (executionId: string) => {
      setDebugLoadingId(executionId);
      try {
        const snapshot = await queryApi.getExecutionLlmContext(executionId);
        setDebugContext(snapshot);
      } catch (err: unknown) {
        toast.error(errorMessage(err, t('assistant.llmContextUnavailable')));
      } finally {
        setDebugLoadingId(null);
      }
    },
    [t],
  );

  const workspaceId = activeWorkspace?.id ?? activeLibrary?.workspaceId;
  const libraryId = activeLibrary?.id;

  const loadSessions = useCallback(async () => {
    if (!workspaceId || !libraryId) return;
    try {
      const data = await queryApi.listSessions({ workspaceId, libraryId });
      setSessions(data.map(mapAssistantSession));
    } catch (err: unknown) {
      console.error('Failed to load sessions:', err);
      toast.error(errorMessage(err, t('assistant.loadSessionsFailed')));
    }
  }, [libraryId, t, workspaceId]);

  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  const loadSessionMessages = useCallback(async (sessionId: string) => {
    try {
      const data = await queryApi.getSession(sessionId);
      setMessages((data.messages ?? []).map(mapAssistantMessage));
    } catch {
      setMessages([]);
    }
  }, []);

  const applyTurnResult = useCallback(
    (streamingId: string, result: AssistantTurnExecutionResponse) => {
      const answerText =
        result.responseTurn?.contentText ?? t('assistant.noResponseGenerated');
      const evidence = mapAssistantTurnToEvidence(result);

      setMessages((prev) =>
        prev.map((message) =>
          message.id === streamingId
            ? {
                id: result.responseTurn?.id ?? streamingId,
                role: 'assistant',
                content: answerText,
                timestamp: result.responseTurn?.createdAt ?? message.timestamp,
                executionId: result.responseTurn?.executionId ?? null,
                evidence,
              }
            : message,
        ),
      );
    },
    [t],
  );

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      setActiveSession(sessionId);
      loadSessionMessages(sessionId);
    },
    [loadSessionMessages],
  );

  const handleNewSession = useCallback(() => {
    setActiveSession(null);
    setMessages([]);
  }, []);

  // Auto-scroll triggered only when the number of messages changes — streaming
  // content deltas do not cause a scroll storm. `messages.length` changes
  // only when a new bubble is added, so the effect runs once per bubble
  // instead of hundreds of times per stream. Using `scrollTo` + rAF avoids
  // smooth-scroll jank during rapid updates.
  useEffect(() => {
    if (messages.length === 0) return;
    const frame = requestAnimationFrame(() => {
      messagesEndRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' });
    });
    return () => cancelAnimationFrame(frame);
  }, [messages.length]);

  const handleSend = async () => {
    if (!inputText.trim()) return;
    if (!workspaceId || !libraryId) return;

    const questionText = inputText.trim();
    const now = Date.now();
    const userMsg: AssistantMessage = {
      id: `m-${now}`,
      role: 'user',
      content: questionText,
      timestamp: new Date().toISOString(),
    };
    setMessages((prev) => [...prev, userMsg]);
    setInputText('');
    setIsExecuting(true);

    // Stable ID for the streaming placeholder — created once per send so React
    // sees a consistent key for the bubble across every delta. Previously the
    // ID was `m-stream-${Date.now()}` which the placeholder assigned to itself
    // and re-used correctly, but extracting to a module-level constant makes
    // the contract explicit to readers.
    const streamingId = `m-stream-${now}`;
    let runtimeExecutionId: string | null = null;

    try {
      let sessionId = activeSession;
      if (!sessionId) {
        const session = await queryApi.createSession(workspaceId, libraryId);
        sessionId = session.id;
        setActiveSession(sessionId);
      }

      setMessages((prev) => [
        ...prev,
        {
          id: streamingId,
          role: 'assistant',
          content: '',
          timestamp: new Date().toISOString(),
        },
      ]);

      const result = await queryApi.createTurnWithFallback(sessionId, questionText, {
        onRuntime: (runtime) => {
          if (typeof runtime.runtimeExecutionId === 'string') {
            runtimeExecutionId = runtime.runtimeExecutionId;
          }
        },
        onToolCallStarted: (event) => {
          setMessages((prev) =>
            prev.map((message) =>
              message.id === streamingId
                ? {
                    ...message,
                    toolSteps: [
                      ...(message.toolSteps ?? []),
                      {
                        iteration: event.iteration,
                        callId: event.callId,
                        name: event.name,
                        argumentsPreview: event.argumentsPreview,
                        status: 'running',
                      },
                    ],
                  }
                : message,
            ),
          );
        },
        onToolCallCompleted: (event) => {
          setMessages((prev) =>
            prev.map((message) =>
              message.id === streamingId
                ? {
                    ...message,
                    toolSteps: (message.toolSteps ?? []).map((step) =>
                      step.callId === event.callId
                        ? {
                            ...step,
                            resultPreview: event.resultPreview,
                            isError: event.isError,
                            status: event.isError ? 'error' : 'done',
                          }
                        : step,
                    ),
                  }
                : message,
            ),
          );
        },
        onDelta: (delta) => {
          setMessages((prev) =>
            prev.map((message) =>
              message.id === streamingId
                ? { ...message, content: message.content + delta }
                : message,
            ),
          );
        },
      });

      applyTurnResult(streamingId, result);

      loadSessions();
    } catch (err: unknown) {
      if (!(err instanceof SseTransportUnavailableError) && runtimeExecutionId) {
        try {
          const recovered = await queryApi.recoverTurnAfterStreamFailure(
            runtimeExecutionId,
          );
          if (recovered?.responseTurn?.id || recovered?.responseTurn?.contentText) {
            applyTurnResult(streamingId, recovered);
            setRetryable(null);
            loadSessions();
            return;
          }
        } catch (recoveryErr) {
          console.warn(
            'Failed to recover assistant turn after stream interruption:',
            recoveryErr,
          );
        }
      }
      const rawMessage = errorMessage(err, t('assistant.unknownError'));
      const diagnosis = diagnoseTurnError(err, rawMessage, t);
      setMessages((prev) => [
        ...prev.filter((message) => !message.id.startsWith('m-stream-')),
        {
          id: `m-err-${Date.now()}`,
          role: 'assistant',
          content: t('assistant.sendError', { error: diagnosis }),
          timestamp: new Date().toISOString(),
        },
      ]);
      setRetryable({ question: questionText, diagnosis });
    } finally {
      setIsExecuting(false);
    }
  };

  const handleRetry = () => {
    if (!retryable) return;
    setInputText(retryable.question);
    setRetryable(null);
    // Two-stage UX: refill the input so the user sees what's about to
    // be sent and can edit before confirming. Intentional — transparent
    // retry risks duplicating the turn if the backend did receive it.
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // Memoize the latest-evidence lookup so streaming deltas (which thrash
  // the `messages` array on every chunk) do not force the evidence panel
  // to recompute from scratch. Guarded by a ref-equality + length check —
  // the lookup walks messages in reverse and stops at the first hit, but
  // during streaming only the last assistant bubble is mutating and it
  // has no evidence until `completed`, so this runs at most once per turn.
  const latestEvidence = useMemo<EvidenceBundle | undefined>(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role === 'assistant' && m.evidence) return m.evidence;
    }
    return undefined;
  }, [messages]);

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">{t('assistant.title')}</h1>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <MessageSquare className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t('assistant.noLibrary')}
          </h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm">
            {t('assistant.noLibraryDesc')}
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => navigate('/documents')}
          >
            <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('assistant.goToDocuments')}
          </Button>
        </div>
      </div>
    );
  }

  if (activeLibrary.missingBindingPurposes.includes('query_answer')) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">{t('assistant.title')}</h1>
        </div>
        <div className="empty-state flex-1">
          <div
            className="w-14 h-14 rounded-2xl flex items-center justify-center mb-4"
            style={{
              background: 'hsl(var(--status-warning-bg))',
              boxShadow: 'inset 0 0 0 1px hsl(var(--status-warning-ring) / 0.3)',
            }}
          >
            <AlertTriangle className="h-7 w-7 text-status-warning" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t('assistant.queryNotConfigured')}
          </h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm">
            {t('assistant.queryNotConfiguredDesc')}
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => navigate('/admin')}
          >
            {t('assistant.goToAdmin')}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header flex items-center justify-between">
        <h1 className="text-lg font-bold tracking-tight">{t('assistant.title')}</h1>
        <div className="flex gap-2">
          <Button
            variant="ghost"
            size="sm"
            className="md:hidden"
            onClick={() => setShowSessionRail(!showSessionRail)}
          >
            {t('assistant.sessions')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowEvidence(!showEvidence)}
          >
            {showEvidence ? t('assistant.evidenceOn') : t('assistant.evidenceOff')}
          </Button>
        </div>
      </div>

      <div className="flex-1 flex overflow-hidden">
        <SessionRail
          t={t}
          locale={locale}
          sessions={sessions}
          activeSession={activeSession}
          show={showSessionRail}
          sessionSearch={sessionSearch}
          onSessionSearchChange={setSessionSearch}
          onNewSession={handleNewSession}
          onSelectSession={handleSelectSession}
        />

        {/* Conversation thread */}
        <div className="flex-1 flex flex-col overflow-hidden">
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
                  {STARTER_PROMPT_KEYS.map((key) => {
                    const prompt = t(key);
                    return (
                      <button
                        key={key}
                        className="text-left p-4 rounded-xl border hover:bg-accent/50 hover:shadow-soft transition-all duration-200 text-sm font-medium"
                        onClick={() => setInputText(prompt)}
                      >
                        {prompt}
                      </button>
                    );
                  })}
                </div>
              </div>
            ) : (
              messages.map((msg) => (
                <ChatMessage
                  key={msg.id}
                  t={t}
                  message={msg}
                  onOpenDebug={openDebugFor}
                />
              ))
            )}

            <div ref={messagesEndRef} />
          </div>

          <div
            className="border-t p-3"
            style={{
              background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--card)))',
            }}
          >
            {retryable && (
              <div className="mb-2 flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive">
                <div className="flex-1">
                  <div className="font-medium">{t('assistant.retryTitle')}</div>
                  <div className="mt-0.5 opacity-80">{retryable.diagnosis}</div>
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 shrink-0 text-xs"
                  onClick={handleRetry}
                >
                  {t('assistant.retryAction')}
                </Button>
              </div>
            )}
            <div className="flex items-end gap-2">
              <Textarea
                value={inputText}
                onChange={(e) => setInputText(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={t('assistant.askPlaceholder')}
                className="min-h-[44px] max-h-[120px] resize-none text-sm rounded-xl"
                rows={1}
              />
              <Button
                size="icon"
                className="shrink-0 rounded-xl h-10 w-10"
                onClick={handleSend}
                disabled={isExecuting || !inputText.trim()}
              >
                <Send className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </div>

        {showEvidence && latestEvidence && (
          <EvidencePanel
            t={t}
            evidence={latestEvidence}
            onOpenDocuments={() => navigate('/documents')}
            onOpenGraph={() => navigate('/graph')}
          />
        )}
      </div>

      {debugLoadingId && !debugContext && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/40 backdrop-blur-sm">
          <div className="bg-card border rounded-lg px-4 py-3 flex items-center gap-2 text-sm">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t('assistant.llmContextLoading')}
          </div>
        </div>
      )}

      {debugContext && (
        <LlmContextDebugDialog
          snapshot={debugContext}
          onClose={() => setDebugContext(null)}
        />
      )}
    </div>
  );
}
