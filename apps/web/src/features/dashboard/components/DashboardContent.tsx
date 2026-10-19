import { AlertCircle, ArrowUpRight, FileClock, FileText, Network } from 'lucide-react'
import type { TFunction } from 'i18next'
import type { CSSProperties } from 'react'
import { Fragment } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router-dom'

import { metaLine, formatNumber, formatRelative, formatDocumentSize } from '@/shared/format'
import { graphStatusLabel, webIngestRunStateLabel } from '@/shared/lib/enumLabels'
import type { Locale } from '@/shared/types'
import { Badge, Button, Panel, Progress } from '@/shared/ui'

import type { dashboardAdapter } from '../model/dashboardAdapter'
import {
  dashboardFactMapPresentation,
  dashboardReadinessPresentation,
  dashboardWebRunTone,
} from '../model/dashboardAdapter'

type DashboardData = ReturnType<typeof dashboardAdapter>

const LOCALIZED_ATTENTION_CODES = new Set([
  'bundle_assembly_failures',
  'failed_documents',
  'failed_rebuilds',
  'graph_coverage_gap',
  'queue_not_progressing',
  'retryable_document',
  'vector_index_gap',
])

function attentionText(
  code: string,
  field: 'title' | 'detail',
  t: TFunction,
  values: Record<string, number | string> = {},
): string {
  const localizedCode = LOCALIZED_ATTENTION_CODES.has(code) ? code : 'unknown'
  return t(`dashboard.attentionItem.${localizedCode}.${field}`, values)
}

/// The count a notice speaks about, so its sentence can decline the noun.
/// Passing every metric to every notice printed "1 documents could not be
/// added" — one `count` per notice is what the plural rules need.
function attentionCountFor(
  code: string,
  metrics: DashboardData['metrics'],
): Record<string, number> {
  if (code === 'failed_documents') return { count: metrics.failed }
  if (code === 'graph_coverage_gap') return { count: metrics.graphSparse }
  return {}
}

/// Names only the buckets that hold something. A fixed four-part line spent
/// most of its width on zeros and wrapped onto a second row to say that
/// nothing was canceled and nothing had failed.
function notReadyBreakdown(
  metrics: DashboardData['metrics'],
  locale: Locale,
  t: TFunction,
): string {
  const buckets = [
    ['dashboard.notReadyProcessing', metrics.processing],
    ['dashboard.notReadyQueued', metrics.queued],
    ['dashboard.notReadyFailed', metrics.failed],
    ['dashboard.notReadyCanceled', metrics.canceled],
    ['dashboard.notReadyEmpty', metrics.empty],
  ] as const
  return metaLine(
    ...buckets.map(([key, value]) =>
      value > 0 ? t(key, { count: value, value: formatNumber(value, locale) }) : null,
    ),
  )
}

function percentage(value: number, total: number): number {
  if (total <= 0) return 0
  // Reserve 100% for a complete set, even when a small remainder rounds up.
  return Math.min(value < total ? 99 : 100, Math.max(0, Math.round((value / total) * 100)))
}

function AttentionLink({
  item,
  values,
  t,
}: Readonly<{
  item: {
    code: string
    routePath: string
    severity: 'info' | 'warning' | 'error'
  }
  values: Record<string, number | string>
  t: TFunction
}>) {
  return (
    <Link to={item.routePath} className={`attention-item attention-item--${item.severity}`}>
      <span>
        <AlertCircle size={16} />
      </span>
      <div>
        <strong>{attentionText(item.code, 'title', t, values)}</strong>
        <p>{attentionText(item.code, 'detail', t, values)}</p>
      </div>
      <ArrowUpRight size={15} />
    </Link>
  )
}

/// The "not ready" tile. At zero it links to a list that is empty by
/// construction, so the arrow and the whole affordance promised a destination
/// with nothing in it, under an equally empty breakdown line. Zero is a plain
/// card; anything above it opens the filtered list.
function NotReadyMetric({
  metrics,
  notReady,
  locale,
  t,
}: Readonly<{
  metrics: DashboardData['metrics']
  notReady: number
  locale: Locale
  t: TFunction
}>) {
  const body = (
    <>
      <span>{t('dashboard.notReadyForSearch')}</span>
      <strong>{formatNumber(notReady, locale)}</strong>
      {notReady > 0 && <small>{notReadyBreakdown(metrics, locale, t)}</small>}
      <FileClock size={18} />
    </>
  )

  if (notReady === 0) {
    return <div className="metric-card metric-card--ready">{body}</div>
  }

  return (
    <Link
      to="/documents?status=notready"
      className={`metric-card metric-card--link metric-card--${metrics.failed > 0 ? 'failed' : 'processing'}`}
    >
      {body}
    </Link>
  )
}

/// The attention tile. Three states, because one link for all of them was a
/// dead control in two of them: with nothing raised the card still carried the
/// "open me" arrow and an aria-label that counted zero notices, and with
/// several raised the in-page anchor scrolled nowhere because the notices panel
/// is already on screen. Nothing is a flat card, one notice opens the screen
/// that fixes it, and several move focus onto the list that enumerates them.
function AttentionMetric({
  attention,
  locale,
  t,
}: Readonly<{
  attention: DashboardData['attention']
  locale: Locale
  t: TFunction
}>) {
  let tone = 'materials'
  if (attention.some((item) => item.severity === 'warning')) tone = 'warning'
  if (attention.some((item) => item.severity === 'error')) tone = 'failed'
  const body = (
    <>
      <span>{t('dashboard.attention')}</span>
      <strong>{formatNumber(attention.length, locale)}</strong>
      <AlertCircle size={18} />
    </>
  )

  // The notices panel already states "all clear" in full. Repeating it here
  // under a zero left the tile saying the same thing twice on one screen.
  if (attention.length === 0) {
    return <div className="metric-card metric-card--ready">{body}</div>
  }

  const only = attention.length === 1 ? attention[0] : undefined
  if (only) {
    return (
      <Link to={only.routePath} className={`metric-card metric-card--link metric-card--${tone}`}>
        {body}
        <small>{t('dashboard.attentionOpenOne')}</small>
      </Link>
    )
  }

  return (
    <a
      href="#dashboard-attention"
      className={`metric-card metric-card--link metric-card--${tone}`}
      aria-label={t('dashboard.attentionCount', { count: attention.length })}
      onClick={() => {
        document.getElementById('dashboard-attention')?.focus()
      }}
    >
      {body}
      <small>{t('dashboard.attentionHint')}</small>
    </a>
  )
}

/**
 * The populated dashboard. Split out of DashboardPage so the page itself is
 * only "header, which state am I in, render one of them" — every conditional
 * below is about the *content* of a library that has data, and mixing the two
 * put twenty branches in one 500-line function.
 */
export function DashboardContent({
  data,
  locale,
}: Readonly<{ data: DashboardData; locale: Locale }>) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const notReady =
    data.metrics.processing +
    data.metrics.queued +
    data.metrics.failed +
    data.metrics.canceled +
    data.metrics.empty
  const inFlight = data.metrics.processing + data.metrics.queued
  const readinessCoverage = percentage(data.metrics.ready, data.metrics.total)
  const factMapPresentation = dashboardFactMapPresentation(data.metrics, data.graph.status)
  const attention = data.attention
  const lastWebRunTone = data.lastWebRun ? dashboardWebRunTone(data.lastWebRun.state) : null
  const rows = [
    [t('dashboard.readyForSearch'), data.metrics.ready, '/documents?status=ready', 'ready'],
    [t('dashboard.notReadyDocuments'), notReady, '/documents?status=notready', 'processing'],
    [
      t('dashboard.factMapCompleteDocuments'),
      data.metrics.graphReady,
      '/documents?status=graphready',
      'enhanced',
    ],
    // A count drills down into that exact document subset, not all ready
    // documents or a graph canvas that cannot enumerate the counted set.
    [
      t('dashboard.factMapLimitedDocuments'),
      data.metrics.graphSparse,
      '/documents?status=graphgap',
      'sparse',
    ],
  ] as const
  const graphStats = [
    [t('dashboard.concepts'), data.graph.nodes, true],
    [t('dashboard.foundRelationships'), data.graph.edges, true],
    [t('dashboard.factDocsSimple'), data.graph.typedFactDocumentCount, false],
  ] as const

  return (
    <>
      <div className="metric-grid dashboard-metrics">
        <Link to="/documents" className="metric-card metric-card--link metric-card--materials">
          <span>{t('dashboard.materials')}</span>
          <strong>{formatNumber(data.metrics.total, locale)}</strong>
          <small>
            {formatNumber(data.metrics.ready, locale)} {t('dashboard.availableNow')}
          </small>
          <FileText size={18} />
        </Link>
        <NotReadyMetric metrics={data.metrics} notReady={notReady} locale={locale} t={t} />
        <AttentionMetric attention={attention} locale={locale} t={t} />
      </div>
      <div className="dashboard-grid">
        <Panel className="coverage-panel">
          <div className="panel-heading">
            <div>
              <h2>{t('dashboard.memoryReadiness')}</h2>
              <p>
                {t('dashboard.memoryReadinessExplanation', {
                  ready: formatNumber(data.metrics.ready, locale),
                  total: formatNumber(data.metrics.total, locale),
                  enhanced: formatNumber(data.metrics.graphReady, locale),
                  sparse: formatNumber(data.metrics.graphSparse, locale),
                })}
              </p>
              <p>
                {t('dashboard.snapshotReceived')}{' '}
                {formatRelative(data.metrics.recomputedAt, locale, t('common.justNow'))}
              </p>
            </div>
            <Badge tone={factMapPresentation.tone}>{t(factMapPresentation.labelKey)}</Badge>
          </div>
          <div className="panel-body coverage-body">
            <div // NOSONAR - <progress> is a replaced element and cannot render a
              // conic-gradient ring with the percentage centred inside it; the
              // ARIA here is complete and asserted by DashboardPage.test.tsx.
              className="coverage-orbit"
              role="progressbar"
              aria-label={t('dashboard.documentReadinessCoverage')}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={readinessCoverage}
              aria-valuetext={t('dashboard.readyDocumentCount', {
                ready: formatNumber(data.metrics.ready, locale),
                total: formatNumber(data.metrics.total, locale),
              })}
              style={
                {
                  '--coverage-percent': `${readinessCoverage}%`,
                } as CSSProperties
              }
            >
              <div>
                <strong>{readinessCoverage}%</strong>
                <span>{t('dashboard.readyForSearch')}</span>
              </div>
            </div>
            <div className="coverage-list">
              {rows.map(([label, value, href, tone], index) => {
                const content = (
                  <>
                    <span className={`status-dot status-dot--${tone}`} />
                    <span>{label}</span>
                    <Progress
                      value={percentage(value, data.metrics.total)}
                      label={label}
                      tone={tone === 'enhanced' ? 'accent' : tone}
                    />
                    <strong>{formatNumber(value, locale)}</strong>
                  </>
                )
                return (
                  <Fragment key={label}>
                    {index === 2 && inFlight > 0 && (
                      <Link className="coverage-list__inflight" to="/documents?status=inflight">
                        {t('dashboard.inFlightDocuments', {
                          value: formatNumber(inFlight, locale),
                        })}
                      </Link>
                    )}
                    {index === 2 && (
                      <p className="coverage-list__hint">{t('dashboard.optionalRelationships')}</p>
                    )}
                    {value > 0 ? (
                      <Link to={href} className="coverage-row">
                        {content}
                      </Link>
                    ) : (
                      <div className="coverage-row">{content}</div>
                    )}
                  </Fragment>
                )
              })}
              <p className="coverage-list__hint">{t('dashboard.coverageBarBase')}</p>
            </div>
            <details className="technical-details">
              <summary>
                <span>{t('dashboard.technicalDetails')}</span>
                <small>{t('dashboard.technicalDetailsHint')}</small>
              </summary>
              <div className="technical-details__status">
                <span>{t('dashboard.factMapStatus')}</span>
                <strong>{graphStatusLabel(data.graph.status, t)}</strong>
              </div>
              <div className="coverage-stats">
                {graphStats.map(([label, value, canNavigate], index) => {
                  const content = (
                    <>
                      {index === 0 && <Network size={16} />}
                      <span>{label}</span>
                      <strong>{formatNumber(value, locale)}</strong>
                    </>
                  )
                  return canNavigate && value > 0 ? (
                    <Link to="/graph" key={label}>
                      {content}
                    </Link>
                  ) : (
                    <div key={label}>{content}</div>
                  )
                })}
              </div>
            </details>
          </div>
        </Panel>
        <Panel className="memory-status-panel">
          <div className="panel-heading">
            <div>
              <h2>{t('dashboard.noticesTitle')}</h2>
              <p>{t('dashboard.noticesHint')}</p>
            </div>
          </div>
          <div className="attention-list" id="dashboard-attention" tabIndex={-1}>
            {attention.length > 0 ? (
              <section className="attention-section">
                <h3>{t('dashboard.attention')}</h3>
                {attention.map((item, index) => (
                  <AttentionLink
                    item={item}
                    values={attentionCountFor(item.code, data.metrics)}
                    t={t}
                    key={`${item.code}-${item.routePath}-${index}`}
                  />
                ))}
              </section>
            ) : (
              <div className="memory-status-ok">
                <span className="status-dot status-dot--ready" />
                <div>
                  <strong>{t('dashboard.noNotices')}</strong>
                  <p>{t('dashboard.noNoticesDetail')}</p>
                </div>
              </div>
            )}
          </div>
          <div className="web-run">
            <div>
              {lastWebRunTone === 'processing' && <span className="live-dot" />}
              {t('dashboard.webImport')}
            </div>
            {data.lastWebRun ? (
              <>
                <Badge tone={lastWebRunTone ?? undefined}>
                  {webIngestRunStateLabel(data.lastWebRun.state, t)}
                </Badge>
                <strong className="web-run__counts">
                  {formatNumber(data.lastWebRun.processed, locale)} {t('dashboard.processed')} ·{' '}
                  {formatNumber(data.lastWebRun.queued, locale)} {t('dashboard.queued')}
                </strong>
                {data.lastWebRun.lastActivityAt && (
                  <small>
                    {t('dashboard.lastActivity')}{' '}
                    {formatRelative(data.lastWebRun.lastActivityAt, locale, t('common.justNow'))}
                  </small>
                )}
              </>
            ) : (
              <strong>{t('dashboard.noRecentRuns')}</strong>
            )}
          </div>
        </Panel>
      </div>
      <Panel className="recent-panel">
        <div className="panel-heading">
          <div>
            <h2>{t('dashboard.recent')}</h2>
            <p>{t('dashboard.recentHint')}</p>
          </div>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void navigate('/documents')
            }}
          >
            {t('common.viewAll')}
            <ArrowUpRight size={14} />
          </Button>
        </div>
        <div className="recent-docs">
          {data.recentDocuments.map((document) => {
            const readiness = dashboardReadinessPresentation(document.readiness)
            return (
              <Link to={`/documents?documentId=${document.id}`} key={document.id}>
                <span className="file-glyph">
                  <FileText size={15} />
                </span>
                <div>
                  <strong>{document.fileName ?? t('common.notAvailable')}</strong>
                  <small>
                    {metaLine(
                      formatDocumentSize(document.fileSize),
                      formatRelative(document.uploadedAt, locale, t('common.justNow')),
                    )}
                  </small>
                </div>
                {readiness.tone !== 'ready' && (
                  <Badge tone={readiness.tone}>{t(readiness.labelKey)}</Badge>
                )}
                {document.readiness === 'processing' && (
                  <Progress
                    value={document.progressPercent ?? 0}
                    label={t('dashboard.preparingForSearch')}
                  />
                )}
                {document.readiness === 'failed' && (
                  <small className="failure-text">
                    {t(
                      document.canRetry
                        ? 'dashboard.failedDocumentRetry'
                        : 'dashboard.failedDocumentReview',
                    )}
                  </small>
                )}
              </Link>
            )
          })}
        </div>
      </Panel>
    </>
  )
}
