import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Library as LibraryIcon, X } from 'lucide-react';
import { AssistantDebugInspector } from '@/features/assistant/components/AssistantDebugInspector';
import { EvidencePanel } from '@/features/assistant/components/EvidencePanel';
import { SessionRail } from '@/features/assistant/components/SessionRail';
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
    defaultValue: false,
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

  const [evidenceOpen, setEvidenceOpen] = useState(false);

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
    <div className="flex-1 flex overflow-hidden bg-background">
      <SessionRail
        id={SESSION_RAIL_ID}
        t={t}
        locale={locale}
        sessions={sessions}
        activeSession={assistant.activeSession}
        collapsed={sessionRailCollapsed}
        disabled={assistant.isExecuting}
        sessionSearch={assistant.sessionSearch}
        onCollapsedChange={setSessionRailCollapsed}
        onSessionSearchChange={assistant.setSessionSearch}
        onNewSession={assistant.newSession}
        onSelectSession={selectSession}
        onRenameSession={renameSession}
        onDeleteSession={handleDeleteSession}
      />

      <div className="min-w-0 flex-1 flex flex-col overflow-hidden">
        <div className="flex h-12 shrink-0 items-center gap-2 border-b bg-card/60 px-4">
          <LibraryIcon className="h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
          <span className="text-xs text-muted-foreground">{t('assistant.searchingScope')}</span>
          <span className="min-w-0 truncate text-sm font-semibold" title={activeLibrary.name}>
            {activeLibrary.name}
          </span>
        </div>
        <ChatThread
          t={t}
          messages={assistant.messages}
          developerMode={showDebug}
          onStarterPromptSelect={setInputText}
          onOpenEvidence={handleOpenEvidence}
          onInspect={handleInspect}
        />
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

      {showEvidencePanel && (
        <>
          {/* Mobile: slide-over backdrop. On lg+ the panel is an inline pane. */}
          <div
            className="fixed inset-0 z-40 bg-foreground/20 backdrop-blur-[1px] lg:hidden"
            aria-hidden="true"
            onClick={() => setEvidenceOpen(false)}
          />
          <EvidencePanel
            t={t}
            evidence={evidenceForPanel}
            className="fixed inset-y-0 right-0 z-50 w-[88%] max-w-sm border-l bg-background lg:static lg:z-auto lg:w-72 lg:max-w-none xl:w-80"
            onClose={() => setEvidenceOpen(false)}
            onOpenDocuments={() => navigate('/documents')}
            onOpenGraph={() => navigate('/graph')}
          />
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
    </div>
  );
}
