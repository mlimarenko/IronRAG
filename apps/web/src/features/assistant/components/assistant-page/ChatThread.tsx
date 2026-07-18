import { useEffect, useRef } from 'react'
import type { TFunction } from 'i18next'
import { Brain } from 'lucide-react'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import type { AssistantMessage } from '@/shared/types'
import { ChatMessage } from '../ChatMessage'
import { countDistinctSources, STARTER_PROMPT_IDS } from './assistantPageState'

type ChatThreadProps = {
  t: TFunction
  messages: AssistantMessage[]
  developerMode?: boolean
  onStarterPromptSelect: (prompt: string) => void
  onOpenEvidence: (message: AssistantMessage) => void
  onInspect: (executionId: string) => void
}

function messageResponseMs(
  messages: readonly AssistantMessage[],
  index: number,
): number | undefined {
  const message = messages[index]
  if (message?.role !== 'assistant') return undefined
  if (typeof message.durationMs === 'number' && message.durationMs > 0) return message.durationMs
  if (!message.timestamp) return undefined

  const assistantMs = Date.parse(message.timestamp)
  for (let previousIndex = index - 1; previousIndex >= 0; previousIndex -= 1) {
    const previousMessage = messages[previousIndex]
    if (previousMessage?.role !== 'user' || !previousMessage.timestamp) continue
    const delta = assistantMs - Date.parse(previousMessage.timestamp)
    return Number.isFinite(delta) && delta > 0 ? delta : undefined
  }
  return undefined
}

export function ChatThread({
  t,
  messages,
  developerMode,
  onStarterPromptSelect,
  onOpenEvidence,
  onInspect,
}: Readonly<ChatThreadProps>) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const lastMessage = messages[messages.length - 1]
  const scrollSignature = lastMessage
    ? [
        messages.length,
        lastMessage.id,
        lastMessage.content?.length ?? 0,
        lastMessage.activityEvents?.length ?? 0,
        lastMessage.executionId ?? '',
      ].join(':')
    : ''

  useEffect(() => {
    if (messages.length === 0) return
    const frame = requestAnimationFrame(() => {
      const container = scrollRef.current
      if (!container) return
      container.scrollTo({
        top: container.scrollHeight,
        behavior: 'smooth',
      })
    })
    return () => cancelAnimationFrame(frame)
  }, [messages.length, scrollSignature])

  return (
    <div
      ref={scrollRef}
      className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 py-4 sm:px-5"
    >
      {messages.length === 0 ? (
        <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col items-center justify-center py-8 animate-fade-in">
          <WorkbenchEmptyState
            icon={<Brain className="h-7 w-7 text-primary" />}
            title={t('assistant.askQuestion')}
            description={t('assistant.askQuestionDesc')}
            action={
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5 max-w-md w-full">
                {STARTER_PROMPT_IDS.map((id) => {
                  const prompt = t(`assistant.starterPrompts.${id}`)
                  return (
                    <button
                      key={id}
                      className="rounded-lg border px-3 py-2.5 text-left text-sm font-medium transition-colors hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
                      onClick={() => onStarterPromptSelect(prompt)}
                    >
                      {prompt}
                    </button>
                  )
                })}
              </div>
            }
          />
        </div>
      ) : (
        <div className="mx-auto flex w-full max-w-5xl flex-col gap-4">
          {messages.map((message, index) => {
            const responseMs = messageResponseMs(messages, index)
            const executionId = message.executionId ?? undefined
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
            )
          })}
        </div>
      )}
    </div>
  )
}
