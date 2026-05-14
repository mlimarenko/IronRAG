import { memo, useMemo } from 'react';
import type { TFunction } from 'i18next';
import { ChevronLeft, ChevronRight, MessageSquareText, Plus, Search } from 'lucide-react';
import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import type { AssistantSession } from '@/shared/types';

type SessionRailProps = {
  id?: string;
  t: TFunction;
  locale: string;
  sessions: AssistantSession[];
  activeSession: string | null;
  collapsed: boolean;
  disabled?: boolean;
  sessionSearch: string;
  onCollapsedChange: (collapsed: boolean) => void;
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
  collapsed,
  disabled = false,
  sessionSearch,
  onCollapsedChange,
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
      className={`${collapsed ? 'w-12' : 'w-64'} flex shrink-0 flex-col border-r bg-surface-sunken/30 transition-[width] duration-250`}
    >
      <button
        type="button"
        className={`flex h-12 items-center border-b text-sm font-semibold transition-colors hover:bg-accent/50 ${
          collapsed ? 'justify-center' : 'justify-between'
        } ${collapsed ? 'px-0' : 'px-3'}`}
        aria-expanded={!collapsed}
        aria-controls={`${id ?? 'assistant-session-rail'}-content`}
        aria-label={collapsed ? t('assistant.expandSessions') : t('assistant.collapseSessions')}
        onClick={() => onCollapsedChange(!collapsed)}
      >
        {collapsed ? (
          <ChevronRight className="h-4 w-4 text-muted-foreground" />
        ) : (
          <>
            <span className="flex min-w-0 items-center gap-2">
              <MessageSquareText className="h-4 w-4 shrink-0 text-primary" />
              <span className="truncate">{t('assistant.sessions')}</span>
            </span>
            <ChevronLeft className="h-4 w-4 text-muted-foreground" />
          </>
        )}
      </button>

      <div
        id={`${id ?? 'assistant-session-rail'}-content`}
        className={collapsed ? 'hidden' : 'min-h-0 flex-1 overflow-y-auto'}
      >
        <div className="space-y-2 p-3">
          <Button size="sm" className="w-full" onClick={onNewSession} disabled={disabled}>
            <Plus className="mr-1.5 h-3.5 w-3.5" /> {t('assistant.newSession')}
          </Button>
          <div className="relative">
            <Search className="absolute left-2.5 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="h-8 pl-8 text-xs"
              placeholder={t('assistant.searchSessions')}
              value={sessionSearch}
              onChange={(e) => onSessionSearchChange(e.target.value)}
              disabled={disabled}
            />
          </div>
        </div>
        <div className="space-y-0.5 px-2 pb-3">
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
                className={`w-full rounded-xl px-3 py-2.5 text-left text-sm transition-all duration-200 ${
                  activeSession === s.id
                    ? 'border border-border/50 bg-card font-semibold shadow-soft'
                    : 'hover:bg-accent/50 disabled:hover:bg-transparent'
                }`}
              >
                <div className="truncate">{s.title || t('assistant.untitledSession')}</div>
                <div className="mt-0.5 text-[11px] text-muted-foreground">
                  {dateFormatter.format(new Date(s.updatedAt))}
                </div>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

export const SessionRail = memo(SessionRailImpl);
