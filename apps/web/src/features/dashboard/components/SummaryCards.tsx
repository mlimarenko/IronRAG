import { memo } from 'react';
import type { LucideIcon } from 'lucide-react';
import { toneClass, type ToneKey } from "../model/format";

export type SummaryCard = {
  key: string;
  label: string;
  value: string;
  detail: string;
  icon: LucideIcon;
  tone: ToneKey;
  actionPath: string;
};

type SummaryCardsProps = {
  cards: SummaryCard[];
  onNavigate: (path: string) => void;
};

function SummaryCardsImpl({ cards, onNavigate }: SummaryCardsProps) {
  return (
    <div className="grid gap-2 sm:grid-cols-3">
      {cards.map((card) => {
        const Icon = card.icon;
        const tone = toneClass(card.tone);
        return (
          <button
            key={card.key}
            type="button"
            onClick={() => onNavigate(card.actionPath)}
            className="flex w-full items-start gap-2 workbench-surface px-3 py-2 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25 sm:items-center sm:gap-3"
          >
            <div className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-md ${tone.containerClass}`}>
              <Icon className={`h-3.5 w-3.5 ${tone.iconClass}`} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <div className="text-base font-semibold tabular-nums sm:text-lg">
                  {card.value}
                </div>
                <div className="text-xs font-medium leading-4 text-muted-foreground">
                  {card.label}
                </div>
              </div>
              {card.detail ? (
                <div className="mt-0.5 hidden overflow-hidden text-xs leading-4 text-muted-foreground sm:block">
                  {card.detail}
                </div>
              ) : null}
            </div>
          </button>
        );
      })}
    </div>
  );
}

export const SummaryCards = memo(SummaryCardsImpl);
