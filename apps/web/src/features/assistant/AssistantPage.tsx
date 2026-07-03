import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { MessageSquare, PanelLeftOpen, X } from 'lucide-react';
import { AssistantDebugInspector } from '@/features/assistant/components/AssistantDebugInspector';
import { EvidencePanel } from '@/features/assistant/components/EvidencePanel';
import { SessionRail } from '@/features/assistant/components/SessionRail';
import { DataView } from '@/shared/components/layout/DataView';
import { PageShell } from '@/shared/components/layout/PageShell';
import { Button } from '@/shared/components/ui/button';
import { useApp } from '@/shared/contexts/app-context';
import { useDeveloperMode } from '@/shared/contexts/preferences-context';
import { useCan } from '@/shared/auth/useCan';
import { useLocalStorageState } from '@/shared/hooks/useLocalStorageState';
import type { AssistantMessage, EvidenceBundle } from '@/shared/types';
import { NoLibraryState, QueryNotConfiguredState } from './components/assistant-page/AssistantUnavailableState';
import { ChatThread } from './components/assistant-page/ChatThread';
import { Composer } from './components/assistant-page/Composer';
import { useAssistantSession } from './components/assistant-page/useAssistantSession';
import { useSessionOverrides } from './components/assistant-page/useSessionOverrides';

const SESSION_RAIL_ID = 'assistant-session-rail';
const DEBUG_PANEL_DEFAULT_WIDTH = 560;
const DEBUG_PANEL_MIN_WIDTH = 420;
const DEBUG_PANEL_MAX_WIDTH = 960;

function parseBoolean(raw: unknown): boolean {
  return raw === true;
}

function parseDebugPanelWidth(raw: unknown): number {
  const value = typeof raw === 'number' && Number.isFinite(raw)
    ? raw
    : DEBUG_PANEL_DEFAULT_WIDTH;
  return Math.min(DEBUG_PANEL_MAX_WIDTH, Math.max(DEBUG_PANEL_MIN_WIDTH, Math.round(value)));
}

export default function AssistantPage() {
  const { t } = useTranslation();
  const { activeLibrary, activeWorkspace, locale } = useApp();
  const navigate = useNavigate();
  const developerMode = useDeveloperMode();
  const { isAdmin } = useCan();
  // Debug surfaces are an operator/dev tool: only admins or users who flipped
  // developer mode see them, instead of every viewer at core-action weight.
  const showDebug = developerMode || isAdmin;

  const [inputText, setInputText] = useState('');
  const [evidenceMessageId, setEvidenceMessageId] = useState<string | null>(null);
  const [sessionRailCollapsed, setSessionRailCollapsed] = useLocalStorageState({
    key: 'ironrag_assistant_sessions_collapsed',
    defaultValue: true,
    parse: parseBoolean,
  });
  const [debugInspectorOpen, setDebugInspectorOpen] = useLocalStorageState({
    key: 'ironrag_assistant_debug_open',
    defaultValue: false,
    parse: parseBoolean,
  });
  const [debugPanelWidth, setDebugPanelWidth] = useLocalStorageState({
    key: 'ironrag_assistant_debug_width',
    defaultValue: DEBUG_PANEL_DEFAULT_WIDTH,
    parse: parseDebugPanelWidth,
  });
  const workspaceId = activeWorkspace?.id ?? activeLibrary?.workspaceId;
  const libraryScopeKey =
    workspaceId && activeLibrary?.id ? `${workspaceId}:${activeLibrary.id}` : null;
  const assistant = useAssistantSession({ workspaceId, libraryId: activeLibrary?.id, t });
  const { applyOverrides, renameSession, deleteSession } = useSessionOverrides(libraryScopeKey);

  const sessions = useMemo(
    () => applyOverrides(assistant.sessions),
    [applyOverrides, assistant.sessions],
  );

  const { newSession, selectSession, activeSession } = assistant;
  const activeSessionTitle = useMemo(() => {
    if (!activeSession) return t('assistant.newQuestionSession');
    const session = sessions.find((candidate) => candidate.id === activeSession);
    return session?.title || t('assistant.untitledSession');
  }, [activeSession, sessions, t]);
  const [evidenceOpen, setEvidenceOpen] = useState(false);
  const [sessionsOpen, setSessionsOpen] = useState(false);

  const handleNewSession = useCallback(() => {
    newSession();
    setSessionsOpen(false);
  }, [newSession]);

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      selectSession(sessionId);
      setSessionsOpen(false);
    },
    [selectSession],
  );

  const handleDeleteSession = useCallback(
    (sessionId: string) => {
      deleteSession(sessionId);
      if (activeSession === sessionId) newSession();
    },
    [activeSession, deleteSession, newSession],
  );

  const handleSend = useCallback(() => {
    if (assistant.sendQuestion(inputText)) setInputText('');
  }, [assistant, inputText]);

  // Retry now auto-resends the failed question rather than merely repopulating
  // the textarea, matching the "retry" mental model.
  const handleRetry = useCallback(() => {
    const question = assistant.prepareRetry();
    if (question) assistant.sendQuestion(question);
  }, [assistant]);

  const latestAssistantExecutionId = useMemo(() => {
    for (let i = assistant.messages.length - 1; i >= 0; i -= 1) {
      const message = assistant.messages[i];
      if (!message) continue;
      if (message.role === 'assistant' && !message.isStreaming && message.executionId) {
        return message.executionId;
      }
    }
    return null;
  }, [assistant.messages]);

  const latestTurnWallClockMs = useMemo(() => {
    const messages = assistant.messages;
    for (let i = messages.length - 1; i >= 0; i -= 1) {
      const msg = messages[i];
      if (!msg) continue;
      if (msg.role !== 'assistant') continue;
      // Server-authoritative wall-clock; immune to client↔server clock skew.
      if (typeof msg.durationMs === 'number' && msg.durationMs > 0) {
        return msg.durationMs;
      }
      if (msg.timestamp) {
        // Reload path: both timestamps are server-stamped (single clock).
        const assistantMs = Date.parse(msg.timestamp);
        for (let j = i - 1; j >= 0; j -= 1) {
          const prev = messages[j];
          if (prev?.role === 'user' && prev.timestamp) {
            const delta = assistantMs - Date.parse(prev.timestamp);
            return Number.isFinite(delta) && delta > 0 ? delta : undefined;
          }
        }
      }
      return undefined;
    }
    return undefined;
  }, [assistant.messages]);

  const { openDebugFor, setDebugContext, debugContext, debugLoadingId } = assistant;

  // The evidence panel scopes to the chosen message, falling back to the most
  // recent answer with evidence so the panel always has something to show.
  const evidenceForPanel = useMemo<EvidenceBundle | null>(() => {
    if (evidenceMessageId) {
      const match = assistant.messages.find(
        (m: AssistantMessage) => m.id === evidenceMessageId,
      );
      if (match?.evidence) return match.evidence;
    }
    return assistant.latestEvidence ?? null;
  }, [assistant.latestEvidence, assistant.messages, evidenceMessageId]);

  const handleOpenEvidence = useCallback((message: AssistantMessage) => {
    setEvidenceMessageId(message.id);
    setEvidenceOpen(true);
  }, []);

  const handleInspect = useCallback(
    (executionId: string) => {
      setDebugInspectorOpen(true);
      void openDebugFor(executionId);
    },
    [openDebugFor, setDebugInspectorOpen],
  );

  const handleCloseDebug = useCallback(() => {
    setDebugInspectorOpen(false);
    assistant.setDebugError(null);
  }, [assistant, setDebugInspectorOpen]);

  useEffect(() => {
    if (!debugInspectorOpen) {
      return;
    }
    if (!latestAssistantExecutionId) {
      setDebugContext(null);
      return;
    }
    if (
      debugContext?.executionId === latestAssistantExecutionId ||
      debugLoadingId === latestAssistantExecutionId
    ) {
      return;
    }
    void openDebugFor(latestAssistantExecutionId);
  }, [
    debugContext?.executionId,
    debugInspectorOpen,
    debugLoadingId,
    latestAssistantExecutionId,
    openDebugFor,
    setDebugContext,
  ]);

  if (!activeLibrary) return <NoLibraryState t={t} onOpenDocuments={() => navigate('/documents')} />;

  if (activeLibrary.missingBindingPurposes.includes('query_answer')) {
    return <QueryNotConfiguredState t={t} onOpenAdmin={() => navigate('/admin/ai')} />;
  }

  const showEvidencePanel = evidenceOpen && evidenceForPanel != null;

  return (
    <PageShell
      bodyClassName="p-3 md:flex md:gap-3"
    >
      <div className={`relative z-10 hidden ${sessionRailCollapsed ? '' : 'md:flex'}`}>
        <SessionRail
          id={SESSION_RAIL_ID}
          className="overflow-hidden workbench-surface"
          t={t}
          locale={locale}
          sessions={sessions}
          activeSession={assistant.activeSession}
          collapsed={false}
          disabled={assistant.isExecuting}
          sessionSearch={assistant.sessionSearch}
          onCollapsedChange={setSessionRailCollapsed}
          onSessionSearchChange={assistant.setSessionSearch}
          onNewSession={handleNewSession}
          onSelectSession={handleSelectSession}
          onRenameSession={renameSession}
          onDeleteSession={handleDeleteSession}
        />
      </div>

      <DataView
        className="relative z-10 min-w-0 flex-1"
        inspector={
          showEvidencePanel ? (
            <EvidencePanel
              t={t}
              evidence={evidenceForPanel}
              className="h-full bg-card"
              onClose={() => setEvidenceOpen(false)}
              onOpenDocuments={() => navigate('/documents')}
              onOpenGraph={() => navigate('/graph')}
            />
          ) : null
        }
        inspectorCloseLabel={t('assistant.close')}
        inspectorLabel={t('assistant.evidence')}
        inspectorOpen={showEvidencePanel}
        showDrawerHeader={false}
        onInspectorOpenChange={(open) => {
          if (!open) setEvidenceOpen(false);
        }}
      >
        <div className="min-w-0 flex-1 flex flex-col overflow-hidden workbench-surface">
          <div className="flex min-h-14 shrink-0 items-center gap-3 border-b bg-card px-4 text-foreground">
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="-ml-2 h-8 w-8 md:hidden"
              aria-label={t('assistant.sessions')}
              onClick={() => setSessionsOpen(true)}
            >
              <PanelLeftOpen className="h-4 w-4" />
            </Button>
            {sessionRailCollapsed && (
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="-ml-2 hidden h-8 w-8 md:flex"
                aria-label={t('assistant.expandSessions')}
                onClick={() => setSessionRailCollapsed(false)}
              >
                <PanelLeftOpen className="h-4 w-4" />
              </Button>
            )}
            <div className="hidden h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary sm:flex">
              <MessageSquare className="h-5 w-5" aria-hidden="true" />
            </div>
            <div className="min-w-0">
              <div className="section-label">
                {t('assistant.currentSession')}
              </div>
              <div className="min-w-0 truncate text-base font-bold tracking-tight" title={activeSessionTitle}>
                {activeSessionTitle}
              </div>
            </div>
          </div>
          <div className="min-h-0 flex-1 bg-background">
            <ChatThread
              t={t}
              messages={assistant.messages}
              developerMode={showDebug}
              onStarterPromptSelect={setInputText}
              onOpenEvidence={handleOpenEvidence}
              onInspect={handleInspect}
            />
          </div>
          <Composer
            t={t}
            inputText={inputText}
            isExecuting={assistant.isExecuting}
            retryable={assistant.retryable}
            onInputTextChange={setInputText}
            onRetry={handleRetry}
            onSend={handleSend}
          />
        </div>
      </DataView>

      {sessionsOpen && (
        <>
          <div
            className="fixed inset-0 z-40 bg-foreground/20 backdrop-blur-[1px] md:hidden"
            aria-hidden="true"
            onClick={() => setSessionsOpen(false)}
          />
          <div className="fixed inset-y-0 left-0 z-50 flex w-[85%] max-w-xs bg-background shadow-lg md:hidden">
            <SessionRail
              id={`${SESSION_RAIL_ID}-mobile`}
              className="flex h-full w-full"
              t={t}
              locale={locale}
              sessions={sessions}
              activeSession={assistant.activeSession}
              collapsed={false}
              disabled={assistant.isExecuting}
              sessionSearch={assistant.sessionSearch}
              onCollapsedChange={() => setSessionsOpen(false)}
              onSessionSearchChange={assistant.setSessionSearch}
              onNewSession={handleNewSession}
              onSelectSession={handleSelectSession}
              onRenameSession={renameSession}
              onDeleteSession={handleDeleteSession}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute right-2 top-2 h-8 w-8"
              aria-label={t('assistant.collapseSessions')}
              onClick={() => setSessionsOpen(false)}
            >
              <X className="h-4 w-4" />
            </Button>
          </div>
        </>
      )}

      <AssistantDebugInspector
        t={t}
        open={debugInspectorOpen}
        width={debugPanelWidth}
        snapshot={debugContext}
        error={assistant.debugError}
        evidence={assistant.latestEvidence ?? null}
        loading={Boolean(debugLoadingId)}
        turnWallClockMs={latestTurnWallClockMs}
        onClose={handleCloseDebug}
        onWidthChange={setDebugPanelWidth}
      />
    </PageShell>
  );
}
