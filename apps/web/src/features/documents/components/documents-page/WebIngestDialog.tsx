import { CheckCircle2, Compass, FileText, HelpCircle, XCircle } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import type { WebIngestMode } from '@/shared/api'
import { Badge, Button, Modal } from '@/shared/ui'
import {
  isSubdomainBoundaryAvailableForSeed,
  isWebBoundaryPolicy,
} from '../../model/webIngestBoundary'
import {
  buildWebIngestUrlFilter,
  evaluateWebIngestUrlFilter,
  formatWebIngestPattern,
  type WebIngestFilterEvaluation,
} from '../../model/webIngestPatterns'
import type { WebIngestController } from './useWebIngestController'

export function WebIngestDialog({ controller }: Readonly<{ controller: WebIngestController }>) {
  const { t } = useTranslation()
  const {
    crawlAllowPatternsText,
    crawlBlockPatternsText,
    errorMessage,
    materializationAllowPatternsText,
    materializationBlockPatternsText,
  } = controller
  const recursive = controller.crawlMode === 'recursive_crawl'
  const subdomainsAvailable = isSubdomainBoundaryAvailableForSeed(controller.seedUrl)
  const filters = useMemo(() => {
    try {
      return {
        ok: true as const,
        crawl: buildWebIngestUrlFilter(crawlAllowPatternsText, crawlBlockPatternsText),
        materialization: buildWebIngestUrlFilter(
          materializationAllowPatternsText,
          materializationBlockPatternsText,
        ),
      }
    } catch (error) {
      return { ok: false as const, error: errorMessage(error, t('common.error')) }
    }
  }, [
    crawlAllowPatternsText,
    crawlBlockPatternsText,
    errorMessage,
    materializationAllowPatternsText,
    materializationBlockPatternsText,
    t,
  ])
  useEffect(() => {
    if (!subdomainsAvailable && controller.boundaryPolicy === 'same_host_and_subdomains')
      controller.setBoundaryPolicy('same_host')
  }, [controller, subdomainsAvailable])
  const testUrl = controller.ruleTestUrl.trim()
  const crawlResult =
    filters.ok && testUrl ? evaluateWebIngestUrlFilter(testUrl, filters.crawl) : null
  const materializationResult =
    filters.ok && testUrl ? evaluateWebIngestUrlFilter(testUrl, filters.materialization) : null

  return (
    <Modal
      open={controller.addLinkOpen}
      onOpenChange={controller.setAddLinkOpen}
      wide
      title={t('documents.addWebContent')}
      description={t('documents.addWebContentDesc')}
      footer={
        <>
          <Button onClick={() => controller.setAddLinkOpen(false)}>{t('common.cancel')}</Button>
          <Button
            variant="primary"
            disabled={
              !controller.seedUrl.trim() ||
              controller.webIngestLoading ||
              controller.webIngestPolicyLoading ||
              !filters.ok
            }
            onClick={() => void controller.startWebIngest()}
          >
            {controller.webIngestLoading ? t('documents.starting') : t('documents.startIngest')}
          </Button>
        </>
      }
    >
      <div className="web-ingest-form">
        <label className="field field--wide">
          <span>{t('documents.seedUrl')}</span>
          <input
            className="input"
            value={controller.seedUrl}
            onChange={(event) => controller.setSeedUrl(event.target.value)}
            placeholder={t('documents.seedUrlPlaceholder')}
          />
        </label>
        <label className="field">
          <span>{t('documents.mode')}</span>
          <select
            className="input"
            value={controller.crawlMode}
            onChange={(event) => controller.setCrawlMode(event.target.value as WebIngestMode)}
          >
            <option value="single_page">{t('documents.singlePage')}</option>
            <option value="recursive_crawl">{t('documents.recursiveCrawl')}</option>
          </select>
        </label>
        <label className="field">
          <span>{t('documents.boundary')}</span>
          <select
            className="input"
            value={controller.boundaryPolicy}
            onChange={(event) => {
              if (isWebBoundaryPolicy(event.target.value))
                controller.setBoundaryPolicy(event.target.value)
            }}
          >
            <option value="same_host">{t('documents.sameHost')}</option>
            <option value="same_host_and_subdomains" disabled={!subdomainsAvailable}>
              {t('documents.sameHostAndSubdomains')}
            </option>
            <option value="allow_external">{t('documents.allowExternal')}</option>
          </select>
        </label>
        {recursive && (
          <>
            <label className="field">
              <span>{t('documents.maxDepth')}</span>
              <input
                className="input"
                type="number"
                min={1}
                max={100}
                value={controller.maxDepth}
                onChange={(event) => controller.setMaxDepth(event.target.value)}
              />
            </label>
            <label className="field">
              <span>{t('documents.maxPages')}</span>
              <input
                className="input"
                type="number"
                min={1}
                max={10000}
                value={controller.maxPages}
                onChange={(event) => controller.setMaxPages(event.target.value)}
              />
            </label>
          </>
        )}
        {recursive && (
          <FilterEditor
            icon={<Compass size={16} />}
            title={t('documents.crawlFilterTitle')}
            allow={controller.crawlAllowPatternsText}
            block={controller.crawlBlockPatternsText}
            onAllow={controller.setCrawlAllowPatternsText}
            onBlock={controller.setCrawlBlockPatternsText}
          />
        )}
        <FilterEditor
          icon={<FileText size={16} />}
          title={t('documents.materializationFilterTitle')}
          allow={controller.materializationAllowPatternsText}
          block={controller.materializationBlockPatternsText}
          onAllow={controller.setMaterializationAllowPatternsText}
          onBlock={controller.setMaterializationBlockPatternsText}
        />
        <section className="rule-tester field--wide">
          <strong>{t('documents.ruleTesterTitle')}</strong>
          <input
            className="input"
            value={controller.ruleTestUrl}
            onChange={(event) => controller.setRuleTestUrl(event.target.value)}
            placeholder={t('documents.ruleTesterPlaceholder')}
          />
          {!filters.ok && (
            <p className="failure-text">
              {t('documents.ruleTesterRulesInvalid', { error: filters.error })}
            </p>
          )}
          <div>
            {recursive && (
              <RuleResult title={t('documents.crawlFilterTitle')} result={crawlResult} />
            )}
            <RuleResult
              title={t('documents.materializationFilterTitle')}
              result={materializationResult}
            />
          </div>
        </section>
      </div>
    </Modal>
  )
}

function FilterEditor({
  allow,
  block,
  icon,
  onAllow,
  onBlock,
  title,
}: Readonly<{
  allow: string
  block: string
  icon: ReactNode
  onAllow: (value: string) => void
  onBlock: (value: string) => void
  title: string
}>) {
  const { t } = useTranslation()
  return (
    <section className="web-filter-editor field--wide">
      <h3>
        {icon}
        {title}
      </h3>
      <div>
        <label className="field">
          <span>{t('documents.filterAllowPatterns')}</span>
          <textarea
            className="input"
            value={allow}
            onChange={(event) => onAllow(event.target.value)}
            placeholder={t('documents.filterAllowPatternsPlaceholder')}
          />
        </label>
        <label className="field">
          <span>{t('documents.filterBlockPatterns')}</span>
          <textarea
            className="input"
            value={block}
            onChange={(event) => onBlock(event.target.value)}
            placeholder={t('documents.filterBlockPatternsPlaceholder')}
          />
        </label>
      </div>
    </section>
  )
}

// Three outcomes, not two: "no result yet" is a distinct state from a rule
// that ran and rejected the URL, and rendering them the same made an
// un-evaluated tester look like a failing one.
type RuleOutcome = 'unknown' | 'pass' | 'fail'

const RULE_OUTCOME_TONE: Readonly<Record<RuleOutcome, string>> = {
  fail: 'failed',
  pass: 'ready',
  unknown: 'neutral',
}

function ruleOutcome(result: WebIngestFilterEvaluation | null): RuleOutcome {
  if (!result) return 'unknown'
  return result.passes ? 'pass' : 'fail'
}

function RuleResult({
  result,
  title,
}: Readonly<{
  result: WebIngestFilterEvaluation | null
  title: string
}>) {
  const { t } = useTranslation()
  const outcome = ruleOutcome(result)
  let label = t('documents.ruleTesterWaiting')
  let detail = t('documents.ruleTesterEmpty')
  if (result?.status === 'invalid_url') {
    label = t('documents.ruleTesterInvalidBadge')
    detail = t('documents.ruleTesterInvalid')
  } else if (result?.status === 'blocked') {
    label = t('documents.ruleTesterFail')
    detail = t('documents.ruleTesterMatchedBlock', {
      pattern: result.matchedPattern ? formatWebIngestPattern(result.matchedPattern) : '',
    })
  } else if (result?.status === 'no_allow_match') {
    label = t('documents.ruleTesterNoAllowBadge')
    detail = t('documents.ruleTesterNoAllowMatch')
  } else if (result?.status === 'allowed') {
    label = t('documents.ruleTesterPass')
    detail = t('documents.ruleTesterMatchedAllow', {
      pattern: result.matchedPattern ? formatWebIngestPattern(result.matchedPattern) : '',
    })
  } else if (result?.status === 'allowed_without_rules') {
    label = t('documents.ruleTesterPass')
    detail = t('documents.ruleTesterNoRules')
  }
  return (
    <div className="rule-result">
      {outcome === 'unknown' && <HelpCircle size={15} />}
      {outcome === 'pass' && <CheckCircle2 size={15} />}
      {outcome === 'fail' && <XCircle size={15} />}
      <span>
        <strong>{title}</strong>
        <small>{detail}</small>
      </span>
      <Badge tone={RULE_OUTCOME_TONE[outcome]}>{label}</Badge>
    </div>
  )
}
