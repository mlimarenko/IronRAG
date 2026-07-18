import { memo, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import type { TFunction } from 'i18next'
import {
  ChevronLeft,
  ChevronRight,
  MessageSquare,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Trash2,
} from 'lucide-react'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/shared/components/ui/dropdown-menu'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'
import { cn } from '@/shared/lib/utils'
import type { AssistantSession } from '@/shared/types'

type SessionRailProps = Readonly<{
  id?: string
  className?: string
  t: TFunction
  locale: string
  sessions: AssistantSession[]
  activeSession: string | null
  collapsed: boolean
  disabled?: boolean
  loading?: boolean
  sessionSearch: string
  onCollapsedChange: (collapsed: boolean) => void
  onSessionSearchChange: (value: string) => void
  onNewSession: () => void
  onSelectSession: (id: string) => void
  onRenameSession: (id: string, title: string) => void
  onDeleteSession: (id: string) => void
}>

type DateBucketId = 'Today' | 'Yesterday' | 'Earlier'

type SessionGroup = {
  id: DateBucketId
  sessions: AssistantSession[]
}

function startOfDay(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime()
}

/** Buckets a timestamp into Today / Yesterday / Earlier relative to `now`. */
function bucketFor(updatedAt: string, todayStart: number, dayMs: number): DateBucketId {
  const ts = Date.parse(updatedAt)
  if (!Number.isFinite(ts)) return 'Earlier'
  if (ts >= todayStart) return 'Today'
  if (ts >= todayStart - dayMs) return 'Yesterday'
  return 'Earlier'
}

type SessionRowProps = Readonly<{
  t: TFunction
  session: AssistantSession
  active: boolean
  disabled: boolean
  dateLabel: string
  onSelect: () => void
  onRename: (title: string) => void
  onDelete: () => void
}>

function SessionRow({
  t,
  session,
  active,
  disabled,
  dateLabel,
  onSelect,
  onRename,
  onDelete,
}: SessionRowProps) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(session.title || '')
  const inputRef = useRef<HTMLInputElement>(null)
  const title = session.title || t('assistant.untitledSession')

  const commit = () => {
    setEditing(false)
    const next = draft.trim()
    if (next && next !== title) onRename(next)
  }

  const handleEditKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') {
      event.preventDefault()
      commit()
    } else if (event.key === 'Escape') {
      event.preventDefault()
      setEditing(false)
      setDraft(session.title || '')
    }
  }

  if (editing) {
    return (
      <div className="workbench-surface border-primary/40 p-2">
        <Input
          ref={inputRef}
          autoFocus
          maxLength={72}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleEditKeyDown}
          onBlur={commit}
          aria-label={t('assistant.renameSession')}
          className="h-8 text-sm"
        />
      </div>
    )
  }

  return (
    <div
      className={cn(
        'group relative flex items-stretch rounded-xl transition-all duration-200',
        active
          ? 'workbench-surface border-border/50'
          : 'border border-transparent hover:bg-accent/50',
      )}
    >
      <button
        type="button"
        onClick={onSelect}
        disabled={disabled}
        className="min-w-0 flex-1 rounded-xl px-3 py-2.5 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset disabled:cursor-not-allowed"
      >
        <div className={cn('truncate text-sm', active && 'font-semibold')}>{title}</div>
        <div className="mt-0.5 flex items-center gap-1.5 text-2xs text-muted-foreground">
          <span>{dateLabel}</span>
          {session.turnCount > 0 && (
            <>
              <span aria-hidden="true">·</span>
              <span className="tabular-nums">
                {t('assistant.turnCount', { count: session.turnCount })}
              </span>
            </>
          )}
        </div>
      </button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            disabled={disabled}
            aria-label={t('assistant.sessionActions')}
            className="mr-1 flex w-7 shrink-0 items-center justify-center rounded-lg text-muted-foreground opacity-0 transition-opacity duration-150 hover:bg-accent hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover:opacity-100 data-[state=open]:opacity-100 disabled:cursor-not-allowed"
          >
            <MoreHorizontal className="h-4 w-4" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-40">
          <DropdownMenuItem
            onSelect={() => {
              setDraft(session.title || '')
              setEditing(true)
              requestAnimationFrame(() => inputRef.current?.focus())
            }}
          >
            <Pencil className="mr-2 h-3.5 w-3.5" />
            {t('assistant.renameSession')}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onDelete} className="text-destructive focus:text-destructive">
            <Trash2 className="mr-2 h-3.5 w-3.5" />
            {t('assistant.deleteSession')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

function SessionListContent({
  loading,
  sessions,
  filteredSessions,
  emptyState,
  groups,
  t,
  activeSession,
  disabled,
  dateFormatter,
  onSelectSession,
  onRenameSession,
  onDeleteSession,
}: Readonly<{
  loading: boolean
  sessions: AssistantSession[]
  filteredSessions: AssistantSession[]
  emptyState: { title: string; description: string }
  groups: SessionGroup[]
  t: TFunction
  activeSession: string | null
  disabled: boolean
  dateFormatter: Intl.DateTimeFormat
  onSelectSession: (id: string) => void
  onRenameSession: (id: string, title: string) => void
  onDeleteSession: (id: string) => void
}>) {
  if (loading && sessions.length === 0) {
    return (
      <div className="space-y-1.5 px-1">
        {[0, 1, 2, 3].map((i) => (
          <div
            key={i}
            className="h-12 animate-pulse rounded-xl bg-muted/60"
            style={{ animationDelay: `${i * 80}ms` }}
          />
        ))}
      </div>
    )
  }
  if (filteredSessions.length === 0) {
    return <WorkbenchEmptyState title={emptyState.title} description={emptyState.description} />
  }
  return (
    <div className="space-y-3">
      {groups.map((group) => (
        <div key={group.id} className="space-y-0.5">
          <div className="px-2 pb-1 pt-1 section-label font-bold text-muted-foreground/70">
            {t(`assistant.group${group.id}`)}
          </div>
          {group.sessions.map((session) => (
            <SessionRow
              key={session.id}
              t={t}
              session={session}
              active={activeSession === session.id}
              disabled={disabled}
              dateLabel={dateFormatter.format(new Date(session.updatedAt))}
              onSelect={() => onSelectSession(session.id)}
              onRename={(title) => onRenameSession(session.id, title)}
              onDelete={() => onDeleteSession(session.id)}
            />
          ))}
        </div>
      ))}
    </div>
  )
}

function SessionRailImpl({
  id,
  className,
  t,
  locale,
  sessions,
  activeSession,
  collapsed,
  disabled = false,
  loading = false,
  sessionSearch,
  onCollapsedChange,
  onSessionSearchChange,
  onNewSession,
  onSelectSession,
  onRenameSession,
  onDeleteSession,
}: SessionRailProps) {
  const filteredSessions = useMemo(() => {
    if (!sessionSearch.trim()) return sessions
    const q = sessionSearch.toLowerCase()
    return sessions.filter((s) =>
      (s.title || t('assistant.untitledSession')).toLowerCase().includes(q),
    )
  }, [sessions, sessionSearch, t])

  const dateFormatter = useMemo(
    () => new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric' }),
    [locale],
  )

  // Group the filtered list into Today / Yesterday / Earlier buckets. Sessions
  // arrive newest-first from the API, so each bucket preserves that order.
  const groups = useMemo<SessionGroup[]>(() => {
    const dayMs = 86_400_000
    const todayStart = startOfDay(new Date())
    const buckets: Record<DateBucketId, AssistantSession[]> = {
      Today: [],
      Yesterday: [],
      Earlier: [],
    }
    for (const session of filteredSessions) {
      buckets[bucketFor(session.updatedAt, todayStart, dayMs)].push(session)
    }
    return (['Today', 'Yesterday', 'Earlier'] as const)
      .map((id) => ({ id, sessions: buckets[id] }))
      .filter((g) => g.sessions.length > 0)
  }, [filteredSessions])

  const emptyState = sessionSearch.trim()
    ? {
        title: t('assistant.noSessionsMatch'),
        description: t('assistant.noSessionsMatchDesc'),
      }
    : {
        title: t('assistant.noSessions'),
        description: t('assistant.noSessionsDesc'),
      }

  return (
    <div
      id={id}
      className={cn(
        'shrink-0 flex-col border-r bg-background transition-[width] duration-250',
        collapsed ? 'w-12' : 'w-64',
        className,
      )}
    >
      <button
        type="button"
        className={cn(
          'flex h-12 items-center border-b text-sm font-semibold transition-colors hover:bg-accent/50',
          collapsed ? 'justify-center px-0' : 'justify-between px-3',
        )}
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
              <MessageSquare className="h-4 w-4 shrink-0 text-primary" />
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
            <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="h-8 pl-8 text-xs"
              placeholder={t('assistant.searchSessions')}
              value={sessionSearch}
              onChange={(e) => onSessionSearchChange(e.target.value)}
              disabled={disabled}
            />
          </div>
        </div>

        <div className="px-2 pb-3">
          <SessionListContent
            loading={loading}
            sessions={sessions}
            filteredSessions={filteredSessions}
            emptyState={emptyState}
            groups={groups}
            t={t}
            activeSession={activeSession}
            disabled={disabled}
            dateFormatter={dateFormatter}
            onSelectSession={onSelectSession}
            onRenameSession={onRenameSession}
            onDeleteSession={onDeleteSession}
          />
        </div>
      </div>
    </div>
  )
}

export const SessionRail = memo(SessionRailImpl)
