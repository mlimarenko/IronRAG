import { Edit3, MessageSquarePlus, PanelLeftClose, Search, Trash2 } from 'lucide-react'
import { useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { usePreferences } from '@/app/preferences-context'
import { Button, IconButton, Modal, ScreenState } from '@/shared/ui'
import type { AssistantSession } from '@/shared/types'

type SessionBucket = 'today' | 'yesterday' | 'earlier'

// Spelled out rather than assembled as `assistant.${bucket}`: a key built from
// a template makes every sibling in the namespace look reachable to the i18n
// audit, which is how the assistant namespace accumulated dead entries.
const SESSION_BUCKET_LABEL_KEYS: Readonly<Record<SessionBucket, string>> = {
  today: 'assistant.today',
  yesterday: 'assistant.yesterday',
  earlier: 'assistant.earlier',
}

const SESSION_BUCKETS = [
  'today',
  'yesterday',
  'earlier',
] as const satisfies readonly SessionBucket[]

type SessionRailProps = Readonly<{
  activeId: string | null
  canManage: boolean
  disabled: boolean
  embedded?: boolean
  error: boolean
  loading: boolean
  locale: string
  search: string
  sessions: AssistantSession[]
  onDelete: (id: string) => void
  onNew: () => void
  onRename: (id: string, title: string) => void
  onRetry: () => void
  onSearchChange: (value: string) => void
  onSelect: (id: string) => void
}>

const DAY_MS = 86_400_000

function sessionBucket(updatedAt: string, now = new Date()): SessionBucket {
  const timestamp = Date.parse(updatedAt)
  if (!Number.isFinite(timestamp)) return 'earlier'
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  if (timestamp >= todayStart) return 'today'
  if (timestamp >= todayStart - DAY_MS) return 'yesterday'
  return 'earlier'
}

function relativeSessionTime(updatedAt: string, locale: string): string {
  const updatedMs = Date.parse(updatedAt)
  if (!Number.isFinite(updatedMs)) return ''
  const elapsedMs = updatedMs - Date.now()
  const elapsedMinutes = Math.round(elapsedMs / 60_000)
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })
  if (Math.abs(elapsedMinutes) < 60) return formatter.format(elapsedMinutes, 'minute')
  const elapsedHours = Math.round(elapsedMinutes / 60)
  if (Math.abs(elapsedHours) < 24) return formatter.format(elapsedHours, 'hour')
  return formatter.format(Math.round(elapsedHours / 24), 'day')
}

export function SessionRail({
  activeId,
  canManage,
  disabled,
  embedded = false,
  error,
  loading,
  locale,
  search,
  sessions,
  onDelete,
  onNew,
  onRename,
  onRetry,
  onSearchChange,
  onSelect,
}: SessionRailProps) {
  const { t } = useTranslation()
  const preferences = usePreferences()
  const disabledHintId = useId()
  const [editingId, setEditingId] = useState<string | null>(null)
  const [renameDraft, setRenameDraft] = useState('')
  const [deleteTarget, setDeleteTarget] = useState<AssistantSession | null>(null)
  const disabledExplanation = disabled ? t('assistant.sessionActionsDisabled') : undefined
  const disabledExplanationId = disabled ? `${disabledHintId}-session-actions-disabled` : undefined
  const filteredSessions = useMemo(() => {
    const query = search.trim().toLocaleLowerCase(locale)
    if (!query) return sessions
    return sessions.filter((session) =>
      (session.title || t('assistant.untitledSession')).toLocaleLowerCase(locale).includes(query),
    )
  }, [locale, search, sessions, t])
  const groups = useMemo(
    () =>
      SESSION_BUCKETS.map((id) => ({
        id,
        sessions: filteredSessions.filter((session) => sessionBucket(session.updatedAt) === id),
      })),
    [filteredSessions],
  )

  if (preferences.sessionRailCollapsed && !embedded) {
    return (
      <aside className="session-rail session-rail--collapsed">
        <IconButton
          label={t('shell.expand')}
          onClick={() => preferences.setSessionRailCollapsed(false)}
        >
          <PanelLeftClose size={17} />
        </IconButton>
        <IconButton
          label={t('assistant.newSession')}
          disabled={disabled}
          title={disabledExplanation}
          onClick={onNew}
        >
          <MessageSquarePlus size={17} />
        </IconButton>
      </aside>
    )
  }

  const startRename = (session: AssistantSession) => {
    setEditingId(session.id)
    setRenameDraft(session.title || t('assistant.untitledSession'))
  }

  const submitRename = (session: AssistantSession) => {
    const title = renameDraft.trim()
    if (title && title !== session.title) onRename(session.id, title)
    setEditingId(null)
  }

  return (
    <>
      <aside className={`session-rail ${embedded ? 'session-rail--embedded' : ''}`}>
        <div className="session-rail__heading">
          <strong>{t('assistant.sessions')}</strong>
          {!embedded && (
            <IconButton
              label={t('shell.collapse')}
              onClick={() => preferences.setSessionRailCollapsed(true)}
            >
              <PanelLeftClose size={16} />
            </IconButton>
          )}
        </div>
        <Button
          variant="primary"
          disabled={disabled}
          title={disabledExplanation}
          aria-describedby={disabledExplanationId}
          onClick={onNew}
        >
          <MessageSquarePlus size={15} />
          {t('assistant.newSession')}
        </Button>
        {disabledExplanation && (
          <output className="field-hint" id={disabledExplanationId}>
            {disabledExplanation}
          </output>
        )}
        <label className="search-box session-search">
          <Search size={15} />
          <input
            className="input"
            aria-label={t('assistant.searchSessions')}
            value={search}
            placeholder={t('assistant.searchSessions')}
            onChange={(event) => onSearchChange(event.target.value)}
          />
        </label>

        {loading && sessions.length === 0 && (
          <div className="session-group" aria-live="polite">
            <span>{t('common.loading')}</span>
          </div>
        )}
        {error && sessions.length === 0 && (
          <div className="verification-warnings" role="alert">
            <strong>{t('assistant.loadSessionsFailed')}</strong>
            <Button size="sm" onClick={onRetry}>
              {t('common.tryAgain')}
            </Button>
          </div>
        )}
        {!loading && !error && filteredSessions.length === 0 && (
          <ScreenState
            scenario="empty"
            emptyText={search.trim() ? t('assistant.noSessionsMatch') : t('assistant.noSessions')}
            emptyHint={
              search.trim() ? t('assistant.noSessionsMatchDesc') : t('assistant.noSessionsDesc')
            }
          >
            {null}
          </ScreenState>
        )}

        {groups.map((group) =>
          group.sessions.length > 0 ? (
            <div className="session-group" key={group.id}>
              <span>{t(SESSION_BUCKET_LABEL_KEYS[group.id])}</span>
              {group.sessions.map((session) => {
                const isEditing = editingId === session.id
                const relativeTime = relativeSessionTime(session.updatedAt, locale)
                return (
                  <div
                    className={`session-row ${session.id === activeId ? 'is-active' : ''}`}
                    key={session.id}
                  >
                    {isEditing ? (
                      <form
                        onSubmit={(event) => {
                          event.preventDefault()
                          submitRename(session)
                        }}
                      >
                        <input
                          autoFocus
                          className="input"
                          aria-label={t('assistant.renameSession')}
                          value={renameDraft}
                          onChange={(event) => setRenameDraft(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === 'Escape') {
                              event.preventDefault()
                              setEditingId(null)
                            }
                          }}
                        />
                      </form>
                    ) : (
                      <button
                        className="session-row__select"
                        type="button"
                        disabled={disabled}
                        title={disabledExplanation}
                        aria-describedby={disabledExplanationId}
                        onClick={() => onSelect(session.id)}
                      >
                        <strong>{session.title || t('assistant.untitledSession')}</strong>
                        <small>
                          {t('assistant.turnCount', { count: session.turnCount })}
                          {relativeTime ? ` · ${relativeTime}` : ''}
                        </small>
                      </button>
                    )}
                    {canManage && !isEditing && (
                      <span className="session-actions">
                        <IconButton
                          label={t('assistant.renameSession')}
                          disabled={disabled}
                          title={disabledExplanation}
                          aria-describedby={disabledExplanationId}
                          onClick={() => startRename(session)}
                        >
                          <Edit3 size={13} />
                        </IconButton>
                        <IconButton
                          label={t('assistant.deleteSession')}
                          disabled={disabled}
                          title={disabledExplanation}
                          aria-describedby={disabledExplanationId}
                          onClick={() => setDeleteTarget(session)}
                        >
                          <Trash2 size={13} />
                        </IconButton>
                      </span>
                    )}
                  </div>
                )
              })}
            </div>
          ) : null,
        )}
      </aside>
      <Modal
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
        title={t('assistant.deleteSession')}
        description={
          deleteTarget
            ? t('assistant.deleteSessionConfirm', {
                title: deleteTarget.title || t('assistant.untitledSession'),
              })
            : undefined
        }
        footer={
          <>
            <Button onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
            <Button
              variant="danger"
              disabled={disabled}
              title={disabledExplanation}
              onClick={() => {
                if (!deleteTarget) return
                onDelete(deleteTarget.id)
                setDeleteTarget(null)
              }}
            >
              {t('common.delete')}
            </Button>
          </>
        }
      >
        <span />
      </Modal>
    </>
  )
}
