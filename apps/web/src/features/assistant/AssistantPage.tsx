import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Wrench } from 'lucide-react';
import { McpToolsPanel } from '@/features/assistant/components/McpToolsPanel';
import { SessionRail } from '@/features/assistant/components/SessionRail';
import { useApp } from '@/shared/contexts/app-context';
import { Button } from '@/shared/components/ui/button';
import { AssistantDebugContext } from './components/assistant-page/AssistantDebugContext';
import { NoLibraryState, QueryNotConfiguredState } from './components/assistant-page/AssistantUnavailableState';
import { ChatThread } from './components/assistant-page/ChatThread';
import { Composer } from './components/assistant-page/Composer';
import { useAssistantSession } from './components/assistant-page/useAssistantSession';

const SESSION_RAIL_ID = 'assistant-session-rail';

export default function AssistantPage() {
  const { t } = useTranslation();
  const { activeLibrary, activeWorkspace, locale } = useApp();
  const navigate = useNavigate();
  const [inputText, setInputText] = useState('');
  const [showMcpTools, setShowMcpTools] = useState(false);
  const [showSessionRail, setShowSessionRail] = useState(true);
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

  useEffect(() => {
    if (!showMcpTools) {
      return;
    }
    if (!latestAssistantExecutionId) {
      setDebugContext(null);
      return;
    }
    if (debugContext?.executionId === latestAssistantExecutionId) {
      return;
    }
    void openDebugFor(latestAssistantExecutionId);
  }, [showMcpTools, latestAssistantExecutionId, openDebugFor, setDebugContext, debugContext]);

  if (!activeLibrary) return <NoLibraryState t={t} onOpenDocuments={() => navigate('/documents')} />;

  if (activeLibrary.missingBindingPurposes.includes('query_answer')) {
    return <QueryNotConfiguredState t={t} onOpenAdmin={() => navigate('/admin?tab=ai')} />;
  }

  const mcpToggleLabel = t('assistant.mcpToolsTitle');

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header flex items-center justify-between">
        <h1 className="text-lg font-bold tracking-tight">{t('assistant.title')}</h1>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            className="md:hidden"
            aria-controls={SESSION_RAIL_ID}
            aria-expanded={showSessionRail}
            onClick={() => setShowSessionRail(!showSessionRail)}
          >
            {t('assistant.sessions')}
          </Button>
          <button
            type="button"
            role="switch"
            aria-checked={showMcpTools}
            aria-label={mcpToggleLabel}
            onClick={() => setShowMcpTools(value => !value)}
            className={`group inline-flex items-center gap-2 rounded-full border px-3 py-1.5 text-xs font-semibold transition-colors ${
              showMcpTools
                ? 'border-primary bg-primary text-primary-foreground shadow-sm'
                : 'border-border/70 bg-background text-muted-foreground hover:border-primary/50 hover:text-foreground'
            }`}
          >
            <Wrench className="h-3.5 w-3.5" />
            <span>{mcpToggleLabel}</span>
            <span
              className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors ${
                showMcpTools ? 'bg-primary-foreground/30' : 'bg-muted'
              }`}
            >
              <span
                className={`inline-block h-3 w-3 transform rounded-full bg-background shadow transition-transform ${
                  showMcpTools ? 'translate-x-3.5' : 'translate-x-0.5'
                }`}
              />
            </span>
          </button>
        </div>
      </div>

      <div className="flex-1 flex overflow-hidden">
        <SessionRail
          id={SESSION_RAIL_ID}
          t={t}
          locale={locale}
          sessions={assistant.sessions}
          activeSession={assistant.activeSession}
          show={showSessionRail}
          disabled={assistant.isExecuting}
          sessionSearch={assistant.sessionSearch}
          onSessionSearchChange={assistant.setSessionSearch}
          onNewSession={assistant.newSession}
          onSelectSession={assistant.selectSession}
        />

        <div className="flex-1 flex flex-col overflow-hidden">
          <ChatThread
            t={t}
            messages={assistant.messages}
            onStarterPromptSelect={setInputText}
            onOpenDebug={assistant.openDebugFor}
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

        {showMcpTools && (
          <McpToolsPanel
            t={t}
            snapshot={debugContext}
            evidence={assistant.latestEvidence ?? null}
            loading={Boolean(debugLoadingId) && debugContext?.executionId !== latestAssistantExecutionId}
          />
        )}
      </div>

      <AssistantDebugContext
        t={t}
        loadingId={!showMcpTools ? assistant.debugLoadingId : null}
        snapshot={!showMcpTools ? assistant.debugContext : null}
        onClose={() => assistant.setDebugContext(null)}
      />
    </div>
  );
}
