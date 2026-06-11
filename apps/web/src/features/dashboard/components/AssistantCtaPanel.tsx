import { memo } from 'react';
import type { TFunction } from 'i18next';
import { ArrowRight, Lock, MessageSquare } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';

type AssistantCtaPanelProps = {
  t: TFunction;
  libraryName: string;
  queryReady: boolean;
  onNavigate: (path: string) => void;
};

/**
 * Turns the dashboard into a real exit toward the product's main verb (DSH-02):
 * a focused "ask about this library" card. When the library isn't query-ready
 * yet (missing AI bindings) it degrades to an honest disabled state explaining
 * why, instead of a CTA that would dead-end.
 */
function AssistantCtaPanelImpl({ t, libraryName, queryReady, onNavigate }: AssistantCtaPanelProps) {
  return (
    <div className="workbench-surface relative overflow-hidden p-5 sm:p-6">
      <div
        className="pointer-events-none absolute -right-8 -top-10 h-32 w-32 rounded-full opacity-60 blur-2xl"
        style={{ background: 'hsl(var(--ambient-glow) / 0.12)' }}
        aria-hidden="true"
      />
      <div className="relative flex items-start gap-3">
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-accent-subtle text-primary ring-1 ring-border/60">
          <MessageSquare className="h-4 w-4" />
        </div>
        <div className="min-w-0 flex-1">
          <h2 className="text-sm font-bold tracking-tight">{t('dashboard.assistantCtaTitle')}</h2>
          <p className="mt-1.5 text-xs leading-relaxed text-muted-foreground">
            {queryReady
              ? t('dashboard.assistantCtaReady', { name: libraryName })
              : t('dashboard.assistantCtaNotReady')}
          </p>
        </div>
      </div>

      <Button
        size="sm"
        variant={queryReady ? 'default' : 'outline'}
        disabled={!queryReady}
        onClick={() => onNavigate('/assistant')}
        className="relative mt-4 w-full justify-between"
      >
        <span className="flex items-center gap-1.5">
          {queryReady ? (
            <MessageSquare className="h-3.5 w-3.5" />
          ) : (
            <Lock className="h-3.5 w-3.5" />
          )}
          {queryReady ? t('dashboard.askAssistant') : t('dashboard.assistantCtaLocked')}
        </span>
        {queryReady && <ArrowRight className="h-3.5 w-3.5" />}
      </Button>
    </div>
  );
}

export const AssistantCtaPanel = memo(AssistantCtaPanelImpl);
