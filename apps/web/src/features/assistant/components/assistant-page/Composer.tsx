import { useEffect, useLayoutEffect, useRef, type KeyboardEvent } from 'react'
import type { TFunction } from 'i18next'
import { RefreshCw, Send } from 'lucide-react'
import { Button } from '@/shared/components/ui/button'
import { Textarea } from '@/shared/components/ui/textarea'
import type { RetryableAssistantTurn } from './assistantPageState'

const COMPOSER_MIN_HEIGHT = 44
const COMPOSER_MAX_HEIGHT = 240

type ComposerProps = Readonly<{
  t: TFunction
  inputText: string
  isExecuting: boolean
  retryable: RetryableAssistantTurn | null
  onInputTextChange: (value: string) => void
  onRetry: () => void
  onSend: () => void
}>

export function Composer({
  t,
  inputText,
  isExecuting,
  retryable,
  onInputTextChange,
  onRetry,
  onSend,
}: ComposerProps) {
  const canSend = !isExecuting && inputText.trim().length > 0
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // Auto-grow: reset to the minimum then expand to fit content, capped so the
  // composer never eats the whole conversation. Beyond the cap, scroll.
  useLayoutEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    const next = Math.min(COMPOSER_MAX_HEIGHT, Math.max(COMPOSER_MIN_HEIGHT, el.scrollHeight))
    el.style.height = `${next}px`
    el.style.overflowY = el.scrollHeight > COMPOSER_MAX_HEIGHT ? 'auto' : 'hidden'
  }, [inputText])

  // Keep focus available for fast retry-and-resend loops.
  useEffect(() => {
    if (!isExecuting) textareaRef.current?.focus()
  }, [isExecuting])

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      if (canSend) onSend()
    }
  }

  return (
    <div className="relative z-10 shrink-0 border-t bg-card/95 px-3 py-3 shadow-soft backdrop-blur sm:px-5">
      <div className="mx-auto w-full max-w-5xl">
        {retryable && (
          <div
            role="alert"
            className="mb-2 flex items-start gap-2 rounded-xl border border-destructive/40 bg-destructive/5 px-3 py-2.5 text-xs text-destructive"
          >
            <div className="min-w-0 flex-1">
              <div className="font-semibold">{t('assistant.retryTitle')}</div>
              <div className="mt-0.5 break-words opacity-80">{retryable.diagnosis}</div>
            </div>
            <Button
              size="sm"
              variant="outline"
              className="h-7 shrink-0 gap-1.5 text-xs"
              onClick={onRetry}
              disabled={isExecuting}
            >
              <RefreshCw className="h-3.5 w-3.5" aria-hidden="true" />
              {t('assistant.retryAction')}
            </Button>
          </div>
        )}
        <div className="flex items-end gap-2">
          <Textarea
            ref={textareaRef}
            aria-label={t('assistant.askPlaceholder')}
            value={inputText}
            onChange={(event) => onInputTextChange(event.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={t('assistant.askPlaceholder')}
            className="min-h-[44px] resize-none rounded-xl text-sm"
            rows={1}
          />
          <Button
            size="icon"
            className="h-10 w-10 shrink-0 rounded-xl"
            aria-label={t('assistant.send')}
            title={t('assistant.send')}
            onClick={onSend}
            disabled={!canSend}
          >
            <Send className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  )
}
