import {
  BrainCircuit,
  CircleDot,
  Clipboard,
  Clock3,
  Database,
  FileSearch,
  LoaderCircle,
  Wrench,
} from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import type { AssistantEvidenceView } from '@/features/assistant/model/assistantAdapter'
import { isInProgressAssistantTurn } from '@/features/assistant/components/assistant-page/assistantPageState'
import { VERIFICATION_TONE, verificationLabel } from '@/features/assistant/model/verificationConfig'
import { useApp } from '@/shared/contexts/app-context'
import { humanizeToken } from '@/shared/lib/enumLabels'
import { Badge, Button, CopyButton } from '@/shared/ui'
import type { AssistantAgentActivityEvent, AssistantMessage } from '@/shared/types'

type MessageBubbleProps = Readonly<{
  isError?: boolean
  message: AssistantMessage
  sourceCount: number
  onClarify: (text: string) => void
  onSources: () => void
  onRetry?: (() => void) | undefined
  retryDisabled?: boolean | undefined
}>

function formatElapsed(ms: number, t: ReturnType<typeof useTranslation>['t']): string {
  const seconds = Math.max(0, Math.floor(ms / 1000))
  if (seconds < 60) return t('assistant.durationSeconds', { count: seconds })
  return t('assistant.durationMinutesSeconds', {
    minutes: Math.floor(seconds / 60),
    seconds: seconds % 60,
  })
}

function userInitials(displayName: string | undefined, login: string | undefined): string {
  const source = displayName?.trim() || login?.trim() || '?'
  return source
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => part[0]?.toLocaleUpperCase() ?? '')
    .join('')
}

function useServerAnchoredElapsed(reportedMs: number | undefined, live: boolean): number {
  const [tickMs, setTickMs] = useState(() => performance.now())
  const [anchor, setAnchor] = useState(() => ({
    reportedMs,
    serverMs: reportedMs ?? 0,
    tickMs,
  }))

  if (reportedMs !== anchor.reportedMs) {
    setAnchor({ reportedMs, serverMs: reportedMs ?? 0, tickMs })
  }

  useEffect(() => {
    if (!live) return undefined
    const timer = window.setInterval(() => setTickMs(performance.now()), 1000)
    return () => window.clearInterval(timer)
  }, [live])

  return Math.max(0, anchor.serverMs + tickMs - anchor.tickMs)
}

function activityIcon(event: AssistantAgentActivityEvent): ReactNode {
  switch (event.type) {
    case 'started':
      return <Clock3 size={15} />
    case 'model_request':
      return <CircleDot size={15} />
    case 'model_response':
      return <BrainCircuit size={15} />
    case 'tool_call_started':
      return <Wrench size={15} />
    case 'tool_call_finished':
      return <FileSearch size={15} />
    case 'persisting':
      return <Database size={15} />
    case 'working':
      return <LoaderCircle className="spin" size={15} />
  }
}

function activityLabel(
  event: AssistantAgentActivityEvent,
  t: ReturnType<typeof useTranslation>['t'],
) {
  switch (event.type) {
    case 'started':
      return t('assistant.activity.started')
    case 'model_request':
      return t('assistant.activity.modelRequest', {
        iteration: event.iteration ?? 1,
        model: event.model_name ?? t('assistant.activity.modelUnknown'),
      })
    case 'model_response':
      return event.has_final_answer
        ? t('assistant.activity.modelFinal')
        : t('assistant.activity.modelToolPlan', { count: event.tool_call_count ?? 0 })
    case 'tool_call_started':
      return t('assistant.activity.toolStarted', {
        tool: event.tool_name
          ? humanizeToken(event.tool_name)
          : t('assistant.activity.toolUnknown'),
      })
    case 'tool_call_finished':
      return t(
        event.is_error ? 'assistant.activity.toolFailed' : 'assistant.activity.toolFinished',
        {
          tool: event.tool_name
            ? humanizeToken(event.tool_name)
            : t('assistant.activity.toolUnknown'),
        },
      )
    case 'persisting':
      return t('assistant.activity.persisting')
    case 'working':
      return t('assistant.activity.working')
  }
}

export function MessageBubble({
  isError = false,
  message,
  sourceCount,
  onClarify,
  onSources,
  onRetry,
  retryDisabled = false,
}: MessageBubbleProps) {
  const { t } = useTranslation()
  const { locale, user } = useApp()
  const isWorking = isInProgressAssistantTurn(message)
  const finishedEmpty = message.role === 'assistant' && !isWorking && !message.content.trim()
  const stalePending =
    finishedEmpty && typeof message.elapsedMs === 'number' && message.elapsedMs >= 0
  // A turn that finished with nothing to show says which kind of nothing it
  // was: one that ran and produced no answer, or one that was left behind by a
  // failure. Reading that as three stacked ternaries hid the distinction.
  function emptyTurnText(): string {
    if (!finishedEmpty) return ''
    return stalePending ? t('assistant.serverFailure') : t('assistant.noResponseGenerated')
  }
  const displayContent = message.content.trim() || emptyTurnText()
  const showAsError = isError || Boolean(displayContent && finishedEmpty)
  const displayedElapsedMs = useServerAnchoredElapsed(message.elapsedMs, isWorking)
  const evidence = message.evidence as AssistantEvidenceView | undefined
  const verificationState = evidence?.verificationState
  const clarification = evidence?.clarification

  return (
    <article
      className={`message message--${message.role}${isWorking ? ' is-working' : ''}${showAsError ? ' is-error' : ''}`}
    >
      <div className="message-meta">
        <span>
          {message.role === 'assistant' ? 'IR' : userInitials(user?.displayName, user?.login)}
        </span>
        <time dateTime={message.timestamp}>
          {new Intl.DateTimeFormat(locale, {
            hour: '2-digit',
            minute: '2-digit',
          }).format(new Date(message.timestamp))}
        </time>
        {verificationState && (
          <Badge tone={VERIFICATION_TONE[verificationState]}>
            {verificationLabel(verificationState, t)}
          </Badge>
        )}
        {/* Both roles: a question is as worth copying as an answer, and the
            control was previously in the answer's footer only. Revealed on
            hover so it does not compete with the message, but focus reveals it
            too, and a coarse pointer keeps it visible — there is no hover to
            wait for on a touchscreen. */}
        {displayContent && !isWorking && (
          <CopyButton
            className="message-copy"
            value={displayContent}
            label={t('assistant.copyMessage')}
            iconOnly
          />
        )}
      </div>
      <div className="message-body">
        {displayContent && !showAsError && (
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{displayContent}</ReactMarkdown>
        )}
        {displayContent && showAsError && (
          <div className="verification-warnings" role="alert">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{displayContent}</ReactMarkdown>
            {onRetry && (
              <Button
                className="message-retry"
                size="sm"
                variant="secondary"
                disabled={retryDisabled}
                onClick={onRetry}
              >
                {t('assistant.retryAction')}
              </Button>
            )}
          </div>
        )}
        {isWorking && (
          <div className="activity-feed" aria-live="polite">
            {message.activityEvents?.length ? (
              message.activityEvents.map((activity, index) => (
                <div key={`${activity.type}-${index}`}>
                  {activityIcon(activity)}
                  <span>{activityLabel(activity, t)}</span>
                </div>
              ))
            ) : (
              <div>
                <LoaderCircle className="spin" size={15} />
                <span>{t('assistant.working')}…</span>
              </div>
            )}
            <div>
              <Clock3 size={15} />
              <span>{formatElapsed(displayedElapsedMs, t)}</span>
            </div>
          </div>
        )}
        {evidence?.verificationWarnings.length ? (
          <div className="verification-warnings">
            {evidence.verificationWarnings.map((warning) => (
              <p key={warning}>{warning}</p>
            ))}
          </div>
        ) : null}
        {clarification?.required && (
          <div className="clarification">
            <strong>{clarification.question ?? t('assistant.clarification')}</strong>
            <div>
              {clarification.answerCandidates.map((candidate) => (
                <button
                  type="button"
                  key={`${candidate.kind}-${candidate.label}`}
                  onClick={() => onClarify(candidate.label)}
                >
                  {candidate.label}
                  <Badge>{candidate.kind}</Badge>
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
      {message.role === 'assistant' && displayContent && !showAsError && (
        <footer className="message-actions">
          {sourceCount > 0 && (
            <Button size="sm" variant="ghost" onClick={onSources}>
              <Clipboard size={14} />
              {t('assistant.seeAllSources', { count: sourceCount })}
            </Button>
          )}
          {message.durationMs !== undefined && (
            <span>
              {t('assistant.messageLatency', { duration: formatElapsed(message.durationMs, t) })}
            </span>
          )}
        </footer>
      )}
    </article>
  )
}
