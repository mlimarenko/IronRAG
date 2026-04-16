import { memo } from 'react';
import type { LucideIcon } from 'lucide-react';
import { toneStyle, type ToneKey } from './format';

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
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {cards.map((card) => {
        const Icon = card.icon;
        const tone = toneStyle(card.tone);
        return (
          <button
            key={card.key}
            type="button"
            onClick={() => onNavigate(card.actionPath)}
            className="stat-tile w-full cursor-pointer text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35"
          >
            <div
              className="w-10 h-10 rounded-xl flex items-center justify-center"
              style={tone.container}
            >
              <Icon className={`h-4 w-4 ${tone.iconClass}`} />
            </div>
            <div className="mt-4">
              <div className="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider">
                {card.label}
              </div>
              <div className="mt-1 text-3xl font-bold tracking-tight tabular-nums">
                {card.value}
              </div>
              <div className="mt-2 text-xs leading-relaxed text-muted-foreground">
                {card.detail}
              </div>
            </div>
          </button>
        );
      })}
    </div>
  );
}

export const SummaryCards = memo(SummaryCardsImpl);
