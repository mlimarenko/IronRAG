import { ExternalLink, Search } from 'lucide-react'
import { useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useQuery } from '@tanstack/react-query'

import { queries, type WebIngestRunListItem, type WebIngestRunPageItem } from '@/shared/api'
import { Badge, Button, CopyButton } from '@/shared/ui'
import { webCandidateStateLabel, webClassificationReasonLabel } from '@/shared/lib/enumLabels'

const TERMINAL = new Set(['completed', 'completed_partial', 'failed', 'canceled'])
const WINDOW = 100

export function WebRunDetails({
  errorMessage,
  run,
}: Readonly<{
  errorMessage: (error: unknown, fallback: string) => string
  run: WebIngestRunListItem
}>) {
  const { t } = useTranslation()
  const headingId = useId()
  const [search, setSearch] = useState('')
  const [state, setState] = useState('all')
  const [offset, setOffset] = useState(0)
  const detail = useQuery({
    ...queries.getContentWebIngestRunOptions({ path: { runId: run.runId } }),
    refetchInterval: (query) =>
      TERMINAL.has(query.state.data?.runState ?? run.runState) ? false : 2500,
  })
  const pages = useQuery({
    ...queries.listContentWebIngestRunPagesOptions({ path: { runId: run.runId } }),
  })
  const allPages = useMemo(() => pages.data ?? [], [pages.data])
  const states = useMemo(
    () => [...new Set(allPages.map((page) => page.candidateState))].sort(),
    [allPages],
  )
  const filtered = useMemo(() => {
    const needle = search.trim().toLowerCase()
    return allPages.filter(
      (page) =>
        (state === 'all' || page.candidateState === state) &&
        (!needle ||
          [pageUrl(page), page.contentType, page.classificationReason]
            .filter(Boolean)
            .join(' ')
            .toLowerCase()
            .includes(needle)),
    )
  }, [allPages, search, state])
  const visible = filtered.slice(offset, offset + WINDOW)
  const counts = detail.data?.counts ?? run.counts

  if (detail.isLoading || pages.isLoading)
    return <div className="inspector-section-state">{t('documents.loadingPages')}</div>
  if (detail.error || pages.error) {
    const message = errorMessage(detail.error ?? pages.error, t('documents.failedToLoad'))
    return (
      <div className="inspector-section-state failure-text" title={message}>
        <span>{message}</span>
        <Button size="sm" onClick={() => void Promise.all([detail.refetch(), pages.refetch()])}>
          {t('documents.retry')}
        </Button>
      </div>
    )
  }
  return (
    <section className="candidate-panel" aria-labelledby={headingId}>
      <h3 id={headingId}>{t('documents.webRunDetailsTitle')}</h3>
      <div className="funnel max-md:grid-cols-1!">
        {(['discovered', 'eligible', 'processed', 'failed'] as const).map((key) => (
          <div key={key}>
            <span>{t(`documents.pageStateLabels.${key}`)}</span>
            <strong>{counts[key].toLocaleString()}</strong>
          </div>
        ))}
      </div>
      <div className="candidate-toolbar max-md:flex-col! max-md:items-stretch!">
        <div className="search-box">
          <Search size={15} />
          <input
            className="input"
            value={search}
            onChange={(event) => {
              setSearch(event.target.value)
              setOffset(0)
            }}
            placeholder={t('documents.pageSearchPlaceholder')}
          />
        </div>
        <select
          className="input max-md:w-full!"
          value={state}
          onChange={(event) => {
            setState(event.target.value)
            setOffset(0)
          }}
        >
          <option value="all">{t('documents.all')}</option>
          {states.map((value) => (
            <option key={value} value={value}>
              {webCandidateStateLabel(value, t)}
            </option>
          ))}
        </select>
      </div>
      <div className="candidate-list">
        {visible.map((page) => {
          const url = pageUrl(page)
          const unavailableMessageId = `web-page-url-unavailable-${page.candidateId}`
          return (
            <div key={page.candidateId}>
              <span className={`candidate-state candidate-state--${page.candidateState}`} />
              <div>
                <strong id={!url ? unavailableMessageId : undefined} title={url || undefined}>
                  {url || t('documents.pageUrlUnavailable')}
                </strong>
                <small>
                  {webCandidateStateLabel(page.candidateState, t)} · {t('documents.maxDepth')}{' '}
                  {page.depth}
                  {page.httpStatus ? ` · HTTP ${page.httpStatus}` : ''}
                  {page.contentType ? ` · ${page.contentType}` : ''}
                </small>
                {page.classificationReason && (
                  <p>
                    <Badge>{webClassificationReasonLabel(page.classificationReason, t)}</Badge>
                  </p>
                )}
              </div>
              <Button
                size="icon"
                variant="ghost"
                disabled={!url}
                aria-describedby={!url ? unavailableMessageId : undefined}
                onClick={() => window.open(url, '_blank', 'noopener,noreferrer')}
                aria-label={t('documents.openPage')}
              >
                <ExternalLink size={14} />
              </Button>
            </div>
          )
        })}
      </div>
      {filtered.length === 0 && (
        <div className="inspector-section-state">{t('documents.noMatchingPages')}</div>
      )}
      {filtered.length > WINDOW && (
        <div className="inspector-pager">
          <Button
            size="sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - WINDOW))}
          >
            {t('documents.previous')}
          </Button>
          <span>
            {t('documents.pageWindowSummary', {
              from: offset + 1,
              to: Math.min(offset + WINDOW, filtered.length),
              total: filtered.length,
            })}
          </span>
          <Button
            size="sm"
            disabled={offset + WINDOW >= filtered.length}
            onClick={() => setOffset(offset + WINDOW)}
          >
            {t('documents.next')}
          </Button>
        </div>
      )}
      <div className="run-id-row">
        <span>{t('documents.runId')}</span>
        <code>{run.runId}</code>
        <CopyButton value={run.runId} />
      </div>
    </section>
  )
}

function pageUrl(page: WebIngestRunPageItem) {
  return page.finalUrl ?? page.canonicalUrl ?? page.normalizedUrl ?? page.discoveredUrl ?? ''
}
