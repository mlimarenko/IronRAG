import type { TFunction } from 'i18next';
import { Loader2 } from 'lucide-react';
import { LlmContextDebugDialog } from '@/features/assistant/components/LlmContextDebugDialog';
import type { LlmContextDebugResponse } from '@/shared/api/query';

type AssistantDebugContextProps = {
  t: TFunction;
  loadingId: string | null;
  snapshot: LlmContextDebugResponse | null;
  onClose: () => void;
};

export function AssistantDebugContext({
  t,
  loadingId,
  snapshot,
  onClose,
}: AssistantDebugContextProps) {
  return (
    <>
      {loadingId && !snapshot && (
        <div
          role="status"
          className="fixed inset-0 z-50 flex items-center justify-center bg-background/40 backdrop-blur-sm"
        >
          <div className="bg-card border rounded-lg px-4 py-3 flex items-center gap-2 text-sm">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t('assistant.llmContextLoading')}
          </div>
        </div>
      )}

      {snapshot && (
        <LlmContextDebugDialog snapshot={snapshot} onClose={onClose} />
      )}
    </>
  );
}
