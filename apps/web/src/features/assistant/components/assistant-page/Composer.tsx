import type { KeyboardEvent } from 'react';
import type { TFunction } from 'i18next';
import { Send } from 'lucide-react';
import { Button } from '@/shared/components/ui/button';
import { Textarea } from '@/shared/components/ui/textarea';
import type { RetryableAssistantTurn } from './assistantPageState';

type ComposerProps = {
  t: TFunction;
  inputText: string;
  isExecuting: boolean;
  retryable: RetryableAssistantTurn | null;
  onInputTextChange: (value: string) => void;
  onRetry: () => void;
  onSend: () => void;
};

export function Composer({
  t,
  inputText,
  isExecuting,
  retryable,
  onInputTextChange,
  onRetry,
  onSend,
}: ComposerProps) {
  const canSend = !isExecuting && inputText.trim().length > 0;

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      if (canSend) onSend();
    }
  };

  return (
    <div
      className="border-t p-3"
      style={{
        background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--card)))',
      }}
    >
      {retryable && (
        <div
          role="alert"
          className="mb-2 flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
        >
          <div className="flex-1">
            <div className="font-medium">{t('assistant.retryTitle')}</div>
            <div className="mt-0.5 opacity-80">{retryable.diagnosis}</div>
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-7 shrink-0 text-xs"
            onClick={onRetry}
          >
            {t('assistant.retryAction')}
          </Button>
        </div>
      )}
      <div className="flex items-end gap-2">
        <Textarea
          aria-label={t('assistant.askPlaceholder')}
          value={inputText}
          onChange={(event) => onInputTextChange(event.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={t('assistant.askPlaceholder')}
          className="min-h-[44px] max-h-[120px] resize-none text-sm rounded-xl"
          rows={1}
        />
        <Button
          size="icon"
          className="shrink-0 rounded-xl h-10 w-10"
          aria-label={t('assistant.send')}
          title={t('assistant.send')}
          onClick={onSend}
          disabled={!canSend}
        >
          <Send className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
