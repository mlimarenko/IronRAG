import { Copy, ExternalLink, Link2, RefreshCw, SquareX } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { WebBoundaryPolicy, WebIngestMode } from '@/shared/api'
import { useApp } from '@/shared/contexts/app-context'
import { formatRelative } from '@/shared/format'
import { Badge, Button, Panel, Progress } from '@/shared/ui'
import { webIngestModeLabel, webIngestRunStateLabel } from '@/shared/lib/enumLabels'
import { formatWebIngestPatterns } from './model/webIngestPatterns'
import type { WebIngestController } from './components/documents-page/useWebIngestController'
import { WebIngestDialog } from './components/documents-page/WebIngestDialog'
import { WebRunDetails } from './components/documents-page/WebRunDetails'

const TERMINAL = new Set(['completed', 'completed_partial', 'failed', 'canceled'])

export function WebIngestPanel({
  controller,
  canWrite,
}: Readonly<{
  controller: WebIngestController
  canWrite: boolean
}>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const [expanded, setExpanded] = useState<string | null>(null)
  const active = controller.webRuns.filter((run) => !TERMINAL.has(run.runState))
  const reuse = (run: (typeof controller.webRuns)[number]) => {
    controller.setSeedUrl(run.seedUrl)
    controller.setCrawlMode(run.mode as WebIngestMode)
    controller.setBoundaryPolicy(run.boundaryPolicy as WebBoundaryPolicy)
    controller.setMaxDepth(String(run.maxDepth))
    controller.setMaxPages(String(run.maxPages))
    controller.setCrawlAllowPatternsText(formatWebIngestPatterns(run.crawlFilter.allowPatterns))
    controller.setCrawlBlockPatternsText(formatWebIngestPatterns(run.crawlFilter.blockPatterns))
    controller.setMaterializationAllowPatternsText(
      formatWebIngestPatterns(run.materializationFilter.allowPatterns),
    )
    controller.setMaterializationBlockPatternsText(
      formatWebIngestPatterns(run.materializationFilter.blockPatterns),
    )
    controller.setAddLinkOpen(true)
  }
  const copy = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url)
      toast.success(t('documents.urlCopied'))
    } catch {
      toast.error(t('documents.urlCopyFailed'))
    }
  }

  return (
    <>
      {active.length > 0 && (
        <div className="web-active-banner">
          <span className="live-dot" />
          <strong>{t('documents.webRunActiveSummary', { count: active.length })}</strong>
        </div>
      )}
      {controller.webRuns.length === 0 ? (
        <Panel className="documents-empty">
          <Link2 size={26} />
          <strong>{t('documents.webIngestRuns')}</strong>
          <p>{t('documents.noWebRunsDesc')}</p>
          {canWrite && (
            <Button variant="primary" onClick={controller.openCreateDialog}>
              {t('documents.addLink')}
            </Button>
          )}
        </Panel>
      ) : (
        <div className="web-run-list">
          {controller.webRuns.map((run) => {
            const open = expanded === run.runId
            const progress = run.counts.discovered
              ? Math.min(100, (run.counts.processed / run.counts.discovered) * 100)
              : 0
            return (
              <Panel className={`run-card ${open ? 'is-expanded' : ''}`} key={run.runId}>
                <div className="run-card__heading max-md:flex-col!">
                  <button type="button" onClick={() => setExpanded(open ? null : run.runId)}>
                    <Badge tone={runTone(run.runState)}>
                      {webIngestRunStateLabel(run.runState, t)}
                    </Badge>
                    <strong>{run.seedUrl}</strong>
                    <p>
                      {webIngestModeLabel(run.mode, t)} · {t('documents.maxDepth')} {run.maxDepth} ·{' '}
                      {t('documents.maxPages')} {run.maxPages}
                    </p>
                  </button>
                  <div className="max-md:w-full max-md:flex-wrap">
                    <Button
                      size="icon"
                      variant="ghost"
                      onClick={() => window.open(run.seedUrl, '_blank', 'noopener,noreferrer')}
                      aria-label={t('documents.openRunUrl')}
                    >
                      <ExternalLink size={14} />
                    </Button>
                    <Button
                      size="icon"
                      variant="ghost"
                      onClick={() => void copy(run.seedUrl)}
                      aria-label={t('documents.copyUrl')}
                    >
                      <Copy size={14} />
                    </Button>
                    {canWrite && (
                      <Button
                        size="icon"
                        variant="ghost"
                        onClick={() => reuse(run)}
                        aria-label={t('documents.reuseRunSettings')}
                      >
                        <RefreshCw size={14} />
                      </Button>
                    )}
                    {canWrite && !TERMINAL.has(run.runState) && (
                      <Button
                        size="icon"
                        variant="ghost"
                        onClick={() => void controller.cancelRun(run.runId)}
                        aria-label={t('documents.cancelRun')}
                      >
                        <SquareX size={14} />
                      </Button>
                    )}
                  </div>
                </div>
                <Progress value={progress} label={t('documents.funnel')} />
                <div className="run-card__footer">
                  <span>
                    {run.lastActivityAt
                      ? formatRelative(run.lastActivityAt, locale, t('common.justNow'))
                      : '—'}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setExpanded(open ? null : run.runId)}
                  >
                    {open ? t('documents.hideDetails') : t('common.details')}
                  </Button>
                </div>
                {open && <WebRunDetails errorMessage={controller.errorMessage} run={run} />}
              </Panel>
            )
          })}
        </div>
      )}
      <WebIngestDialog controller={controller} />
    </>
  )
}

function runTone(state: string) {
  if (state === 'completed') return 'ready'
  if (state === 'failed') return 'failed'
  if (state === 'canceled') return 'canceled'
  if (state === 'completed_partial') return 'warning'
  return 'processing'
}
