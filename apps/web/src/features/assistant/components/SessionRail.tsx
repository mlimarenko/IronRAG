import { memo, useMemo } from 'react';
import type { TFunction } from 'i18next';
import { Plus, Search } from 'lucide-react';
import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import type { AssistantSession } from '@/shared/types';

type SessionRailProps = {
  id?: string;
  t: TFunction;
  locale: string;
  sessions: AssistantSession[];
  activeSession: string | null;
  show: boolean;
  disabled?: boolean;
  sessionSearch: string;
  onSessionSearchChange: (value: string) => void;
  onNewSession: () => void;
  onSelectSession: (id: string) => void;
};

function SessionRailImpl({
  id,
  t,
  locale,
  sessions,
  activeSession,
  show,
  disabled = false,
  sessionSearch,
  onSessionSearchChange,
  onNewSession,
  onSelectSession,
}: SessionRailProps) {
  // Pre-compute the case-insensitive filtered list once per (sessions, search).
  // Previously recomputed on every parent render during streaming.
  const filteredSessions = useMemo(() => {
    if (!sessionSearch.trim()) return sessions;
    const q = sessionSearch.toLowerCase();
    return sessions.filter((s) => (s.title || t('assistant.untitledSession')).toLowerCase().includes(q));
  }, [sessions, sessionSearch, t]);

  // Cache the date formatter so every row does not construct a fresh Intl
  // formatter per render.
  const dateFormatter = useMemo(() => new Intl.DateTimeFormat(locale), [locale]);

  return (
    <div
      id={id}
      className={`${show ? 'w-64' : 'w-0 overflow-hidden'} shrink-0 border-r bg-surface-sunken/30 transition-all duration-250 md:w-64`}
    >
      <div className="p-3 space-y-2">
        <Button size="sm" className="w-full" onClick={onNewSession} disabled={disabled}>
          <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('assistant.newSession')}
        </Button>
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3 w-3 text-muted-foreground" />
          <Input
            className="h-8 pl-8 text-xs"
            placeholder={t('assistant.searchSessions')}
            value={sessionSearch}
            onChange={(e) => onSessionSearchChange(e.target.value)}
            disabled={disabled}
          />
        </div>
      </div>
      <div className="px-2 space-y-0.5">
        {filteredSessions.length === 0 ? (
          <div className="px-3 py-6 text-center">
            <div className="text-sm font-semibold">{t('assistant.noSessions')}</div>
            <div className="mt-1 text-xs leading-relaxed text-muted-foreground">
              {t('assistant.noSessionsDesc')}
            </div>
          </div>
        ) : (
          filteredSessions.map((s) => (
            <button
              key={s.id}
              onClick={() => onSelectSession(s.id)}
              disabled={disabled}
              className={`w-full text-left px-3 py-2.5 rounded-xl text-sm transition-all duration-200 ${
                activeSession === s.id
                  ? 'bg-card shadow-soft font-semibold border border-border/50'
                  : 'hover:bg-accent/50 disabled:hover:bg-transparent'
              }`}
            >
              <div className="truncate">{s.title || t('assistant.untitledSession')}</div>
              <div className="text-[11px] text-muted-foreground mt-0.5">
                {dateFormatter.format(new Date(s.updatedAt))}
              </div>
            </button>
          ))
        )}
      </div>
    </div>
  );
}

export const SessionRail = memo(SessionRailImpl);
