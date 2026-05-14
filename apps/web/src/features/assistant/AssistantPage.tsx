import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { AssistantDebugInspector } from '@/features/assistant/components/AssistantDebugInspector';
import { SessionRail } from '@/features/assistant/components/SessionRail';
import { useApp } from '@/shared/contexts/app-context';
import { useLocalStorageState } from '@/shared/hooks/useLocalStorageState';
import { NoLibraryState, QueryNotConfiguredState } from './components/assistant-page/AssistantUnavailableState';
import { ChatThread } from './components/assistant-page/ChatThread';
import { Composer } from './components/assistant-page/Composer';
import { useAssistantSession } from './components/assistant-page/useAssistantSession';

const SESSION_RAIL_ID = 'assistant-session-rail';
const DEBUG_PANEL_DEFAULT_WIDTH = 380;
const DEBUG_PANEL_MIN_WIDTH = 320;
const DEBUG_PANEL_MAX_WIDTH = 720;

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
  const [inputText, setInputText] = useState('');
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
  const assistant = useAssistantSession({ workspaceId, libraryId: activeLibrary?.id, t });

  const handleSend = useCallback(() => {
    if (assistant.sendQuestion(inputText)) setInputText('');
  }, [assistant, inputText]);

  const handleRetry = useCallback(() => {
    const question = assistant.prepareRetry();
    if (question) setInputText(question);
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

  const { openDebugFor, setDebugContext, debugContext, debugLoadingId } = assistant;

  const handleToggleDebug = useCallback(() => {
    setDebugInspectorOpen((open) => {
      const nextOpen = !open;
      if (nextOpen && latestAssistantExecutionId) {
        void openDebugFor(latestAssistantExecutionId);
      }
      return nextOpen;
    });
  }, [latestAssistantExecutionId, openDebugFor, setDebugInspectorOpen]);

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
    return <QueryNotConfiguredState t={t} onOpenAdmin={() => navigate('/admin?tab=ai')} />;
  }

  return (
    <div className="flex-1 flex overflow-hidden bg-background">
      <SessionRail
        id={SESSION_RAIL_ID}
        t={t}
        locale={locale}
        sessions={assistant.sessions}
        activeSession={assistant.activeSession}
        collapsed={sessionRailCollapsed}
        disabled={assistant.isExecuting}
        sessionSearch={assistant.sessionSearch}
        onCollapsedChange={setSessionRailCollapsed}
        onSessionSearchChange={assistant.setSessionSearch}
        onNewSession={assistant.newSession}
        onSelectSession={assistant.selectSession}
      />

      <div className="min-w-0 flex-1 flex flex-col overflow-hidden">
        <ChatThread
          t={t}
          messages={assistant.messages}
          onStarterPromptSelect={setInputText}
        />
        <Composer
          t={t}
          inputText={inputText}
          isExecuting={assistant.isExecuting}
          debugOpen={debugInspectorOpen}
          debugLoading={Boolean(debugLoadingId)}
          retryable={assistant.retryable}
          onInputTextChange={setInputText}
          onRetry={handleRetry}
          onToggleDebug={handleToggleDebug}
          onSend={handleSend}
        />
      </div>

      <AssistantDebugInspector
        t={t}
        open={debugInspectorOpen}
        width={debugPanelWidth}
        snapshot={debugContext}
        error={assistant.debugError}
        evidence={assistant.latestEvidence ?? null}
        loading={Boolean(debugLoadingId)}
        onClose={handleCloseDebug}
        onWidthChange={setDebugPanelWidth}
      />
    </div>
  );
}
