import {
  AlertTriangle,
  Bug,
  FileText,
  MessageSquarePlus,
  PanelLeft,
  Send,
  Sparkles,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useSearchParams } from 'react-router-dom'

import { useLibraryDeepLink } from '@/shared/hooks/useLibraryDeepLink'
import { useWindowWidth } from '@/shared/hooks/useWindowWidth'

import { usePreferences } from '@/app/preferences-context'
import { ASSISTANT_DEBUG_WIDTH_MAX, ASSISTANT_DEBUG_WIDTH_MIN } from '@/app/PreferencesContext'
// The assistant must use the labels maintained by the AI configuration feature.
// eslint-disable-next-line no-restricted-imports
import { purposeLabel } from '@/features/admin/model/aiConfig'
import { countDistinctSources } from '@/features/assistant/components/assistant-page/assistantPageState'
import { useAssistantSession } from '@/features/assistant/components/assistant-page/useAssistantSession'
import { useCan } from '@/shared/auth/useCan'
import { useApp } from '@/shared/contexts/app-context'
import { Button, Drawer, PageHeader, PanelResizer } from '@/shared/ui'
import type { AssistantMessage } from '@/shared/types'

import { DebugPanel } from './DebugPanel'
import { EvidenceDrawer } from './EvidenceDrawer'
import { MessageBubble } from './MessageBubble'
import { SessionRail } from './SessionRail'

function latestExecutionId(messages: readonly AssistantMessage[]): string | null {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]
    if (!message) continue
    if (message.role === 'user' || !message.content.trim()) return null
    if (message.executionId) return message.executionId
  }
  return null
}

export default function AssistantPage() {
  const { t } = useTranslation()
  const { activeLibrary, activeWorkspace, locale, selectLibrary } = useApp()
  const { can, isAdmin } = useCan()
  const preferences = usePreferences()
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  // A ?lib= link now lands here too, not only on the AI configuration screen.
  useLibraryDeepLink({
    params,
    setParams,
    activeLibraryId: activeLibrary?.id,
    selectLibrary,
  })
  const queryReady = activeLibrary?.queryReady === true
  const assistant = useAssistantSession({
    workspaceId: activeWorkspace?.id ?? activeLibrary?.workspaceId,
    libraryId: queryReady ? activeLibrary.id : undefined,
    t,
  })
  const composerScope = activeLibrary?.id ?? null
  const [composerState, setComposerState] = useState(() => ({
    scope: composerScope,
    value: params.get('prompt') ?? '',
  }))
  const composer = composerState.scope === composerScope ? composerState.value : ''
  const setComposer = useCallback(
    (value: string) => setComposerState({ scope: composerScope, value }),
    [composerScope],
  )
  const [evidenceMessageId, setEvidenceMessageId] = useState<string | null>(null)
  const [evidenceOpen, setEvidenceOpen] = useState(false)
  const [sessionsOpen, setSessionsOpen] = useState(false)
  const [debugOpen, setDebugOpen] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)
  const prompts = [
    t('assistant.promptOverview'),
    t('assistant.promptThemes'),
    t('assistant.promptQuestions'),
    t('assistant.promptChanges'),
  ]
  const canManageSessions = can('library.edit')
  const canDebug = isAdmin || preferences.devMode
  const windowWidth = useWindowWidth()
  const debugVisible = canDebug && debugOpen
  // A width stored on a 4K screen must not swallow a laptop's chat column, so
  // the stored value is clamped against the window it is being applied in.
  const debugWidthMax = Math.max(
    ASSISTANT_DEBUG_WIDTH_MIN,
    Math.min(ASSISTANT_DEBUG_WIDTH_MAX, Math.round(windowWidth * 0.6)),
  )
  const debugWidth = Math.min(
    Math.max(preferences.assistantDebugWidth, ASSISTANT_DEBUG_WIDTH_MIN),
    debugWidthMax,
  )
  const executionId = useMemo(() => latestExecutionId(assistant.messages), [assistant.messages])
  const selectedEvidence = useMemo(
    () => assistant.messages.find((message) => message.id === evidenceMessageId)?.evidence ?? null,
    [assistant.messages, evidenceMessageId],
  )
  const activeSessionTitle = useMemo(() => {
    if (!assistant.activeSession) return activeLibrary?.name ?? t('assistant.newDialog')
    return (
      assistant.sessions.find((session) => session.id === assistant.activeSession)?.title ||
      t('assistant.newDialog')
    )
  }, [activeLibrary?.name, assistant.activeSession, assistant.sessions, t])
  const lastMessage = assistant.messages.at(-1)
  const scrollSignature = `${assistant.messages.length}:${lastMessage?.content.length ?? 0}:${lastMessage?.activityEvents?.length ?? 0}`

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      scrollRef.current?.scrollTo({
        top: scrollRef.current.scrollHeight,
        behavior: 'smooth',
      })
    })
    return () => cancelAnimationFrame(frame)
  }, [scrollSignature])

  useEffect(() => {
    if (!debugVisible || !executionId) return
    if (
      assistant.debugContext?.executionId === executionId ||
      assistant.debugLoadingId === executionId ||
      assistant.debugErrorExecutionId === executionId
    ) {
      return
    }
    void assistant.openDebugFor(executionId)
  }, [assistant, debugVisible, executionId])

  const send = useCallback(() => {
    if (assistant.sendQuestion(composer)) setComposer('')
  }, [assistant, composer, setComposer])

  const retry = useCallback(() => {
    const question = assistant.prepareRetry()
    if (question) assistant.sendQuestion(question)
  }, [assistant])

  const showSources = useCallback((message: AssistantMessage) => {
    if (!message.evidence) return
    setEvidenceMessageId(message.id)
    setEvidenceOpen(true)
  }, [])

  const sessionRailProps = {
    activeId: assistant.activeSession,
    canManage: canManageSessions,
    disabled: assistant.isExecuting || assistant.isSessionMutationPending,
    error: Boolean(assistant.sessionsError),
    loading: assistant.sessionsLoading,
    locale,
    search: assistant.sessionSearch,
    sessions: assistant.sessions,
    onDelete: assistant.deleteSession,
    onNew: () => {
      assistant.newSession()
      setSessionsOpen(false)
    },
    onRename: assistant.renameSession,
    onRetry: () => void assistant.refetchSessions(),
    onSearchChange: assistant.setSessionSearch,
    onSelect: (id: string) => {
      assistant.selectSession(id)
      setSessionsOpen(false)
    },
  }

  if (!activeLibrary) {
    return (
      <div className="state-card">
        <FileText size={22} />
        <div>
          <strong>{t('assistant.noLibrary')}</strong>
          <p>{t('assistant.noLibraryDesc')}</p>
          <Button size="sm" onClick={() => navigate('/documents')}>
            {t('assistant.goToDocuments')}
          </Button>
        </div>
      </div>
    )
  }

  if (!queryReady) {
    return (
      <>
        <PageHeader title={t('assistant.title')} description={t('assistant.subtitle')} />
        <div className="state-card state-card--error" role="alert">
          <AlertTriangle size={22} />
          <div>
            <strong>{t('assistant.queryNotReady')}</strong>
            <p>{t('assistant.queryNotConfiguredHumanDesc')}</p>
            {activeLibrary.missingBindingPurposes.length > 0 && (
              <p>
                {activeLibrary.missingBindingPurposes
                  .map((purpose) => purposeLabel(purpose, t))
                  .join(', ')}
              </p>
            )}
            <Button size="sm" onClick={() => navigate('/admin/ai')}>
              {t('assistant.goToAdmin')}
            </Button>
          </div>
        </div>
      </>
    )
  }

  if (assistant.libraryContentUnavailable) {
    return (
      <div className="state-card state-card--error" role="alert">
        <FileText size={22} />
        <div>
          <strong>{t('assistant.libraryContentUnavailable')}</strong>
          <p>{t('assistant.libraryContentUnavailableDesc')}</p>
          <Button size="sm" onClick={() => navigate('/documents')}>
            {t('assistant.goToDocuments')}
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div
      className={`assistant-layout page-fill ${preferences.sessionRailCollapsed ? 'is-rail-collapsed' : ''} ${debugVisible ? 'has-debug' : ''}`}
      // The width is stored per browser, so the grid reads it from here rather
      // than from a stylesheet that cannot know it.
      style={{ '--assistant-debug-width': `${debugWidth}px` } as CSSProperties}
    >
      <SessionRail {...sessionRailProps} />
      <section className="chat-surface">
        <header className="chat-header">
          <div>
            <strong>{activeSessionTitle}</strong>
          </div>
          <div className="chat-header__actions">
            <Button
              className="mobile-session-trigger"
              size="icon"
              variant="ghost"
              aria-label={t('assistant.sessions')}
              onClick={() => setSessionsOpen(true)}
            >
              <PanelLeft size={17} />
            </Button>
            <Button
              className="mobile-session-trigger"
              size="icon"
              variant="ghost"
              aria-label={t('assistant.newSession')}
              disabled={assistant.isExecuting || assistant.isSessionMutationPending}
              onClick={sessionRailProps.onNew}
            >
              <MessageSquarePlus size={17} />
            </Button>
            {preferences.sessionRailCollapsed && (
              <Button
                className="desktop-session-trigger"
                size="icon"
                variant="ghost"
                aria-label={t('assistant.expandSessions')}
                onClick={() => preferences.setSessionRailCollapsed(false)}
              >
                <PanelLeft size={17} />
              </Button>
            )}
            {canDebug && (
              <Button
                size="sm"
                variant={debugVisible ? 'secondary' : 'ghost'}
                disabled={executionId === null}
                title={executionId === null ? t('assistant.debugInspectorEmpty') : undefined}
                onClick={() => setDebugOpen((open) => !open)}
              >
                <Bug size={14} />
                {t('assistant.debug')}
              </Button>
            )}
          </div>
        </header>
        <div className="message-list" ref={scrollRef}>
          {assistant.messages.length === 0 ? (
            <div className="assistant-empty">
              <span>
                <Sparkles size={24} />
              </span>
              <h2>{t('assistant.emptyTitle')}</h2>
              <p>{t('assistant.subtitle')}</p>
              <div>
                {prompts.map((prompt) => (
                  <button type="button" key={prompt} onClick={() => setComposer(prompt)}>
                    {prompt}
                  </button>
                ))}
              </div>
            </div>
          ) : (
            assistant.messages.map((message, index) => {
              const isFailedTurn =
                Boolean(assistant.retryable) &&
                index === assistant.messages.length - 1 &&
                message.role === 'assistant'
              return (
                <MessageBubble
                  key={message.id}
                  message={message}
                  isError={isFailedTurn}
                  sourceCount={countDistinctSources(message)}
                  onSources={() => showSources(message)}
                  onClarify={setComposer}
                  onRetry={isFailedTurn ? retry : undefined}
                  retryDisabled={assistant.isExecuting}
                />
              )
            })
          )}
        </div>
        <div>
          <div className="composer">
            <textarea
              value={composer}
              onChange={(event) => setComposer(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && !event.shiftKey) {
                  event.preventDefault()
                  send()
                }
              }}
              placeholder={t('assistant.placeholder')}
              aria-label={t('assistant.placeholder')}
              rows={2}
            />
            <Button
              size="icon"
              variant="primary"
              disabled={!composer.trim() || assistant.isExecuting}
              onClick={send}
              aria-label={t('assistant.send')}
            >
              <Send size={17} />
            </Button>
          </div>
        </div>
      </section>
      {debugVisible && (
        <PanelResizer
          value={debugWidth}
          min={ASSISTANT_DEBUG_WIDTH_MIN}
          max={debugWidthMax}
          edge="right"
          label={t('assistant.resizeDebug')}
          onChange={preferences.setAssistantDebugWidth}
        />
      )}
      <DebugPanel
        open={debugVisible}
        onClose={() => setDebugOpen(false)}
        executionId={executionId}
        libraryId={activeLibrary.id}
        context={assistant.debugContext}
        contextLoading={assistant.debugLoadingId === executionId && executionId !== null}
        contextError={assistant.debugErrorExecutionId === executionId ? assistant.debugError : null}
        evidence={assistant.latestEvidence ?? null}
      />
      <EvidenceDrawer
        evidence={selectedEvidence}
        open={evidenceOpen && selectedEvidence !== null}
        onOpenChange={setEvidenceOpen}
      />
      <Drawer open={sessionsOpen} onOpenChange={setSessionsOpen} title={t('assistant.sessions')}>
        <SessionRail {...sessionRailProps} embedded />
      </Drawer>
    </div>
  )
}
