import { useId } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'

import type { SemanticEvidenceLink, SemanticVerificationReport } from '@/shared/api/generated'
import { Badge, Button } from '@/shared/ui'

type ReportProps = Readonly<{ report: SemanticVerificationReport }>

function SourceQuotes({
  evidence,
  report,
}: ReportProps & Readonly<{ evidence: SemanticEvidenceLink[] }>) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  return (
    <ul className="semantic-quotes">
      {evidence.map((link, index) => {
        const source = report.sources.find((entry) => entry.sourceId === link.sourceId)
        return (
          <li key={`${link.sourceId}-${index}`}>
            <blockquote
              className="semantic-exact-text"
              tabIndex={0}
              aria-label={t('assistant.semantic.sourceQuote')}
            >
              {link.quote}
            </blockquote>
            {source ? (
              <>
                <Badge>{t(`assistant.semantic.scope.${source.scope}`)}</Badge>
                <Button
                  variant="ghost"
                  onClick={() =>
                    navigate(`/documents?documentId=${encodeURIComponent(source.documentId)}`)
                  }
                >
                  {t('assistant.openSourceDocument')}
                </Button>
              </>
            ) : null}
          </li>
        )
      })}
    </ul>
  )
}

export function SemanticVerificationDetails({ report }: ReportProps) {
  const { t } = useTranslation()
  const headingId = useId()
  const { outcome } = report
  const differentCandidate =
    report.visibleAnswerSha256 != null && report.visibleAnswerSha256 !== report.checkedAnswerSha256
  const assessment = outcome.status === 'completed' ? outcome.assessment : null

  return (
    <section className="semantic-verification" aria-labelledby={headingId}>
      <h3 id={headingId}>{t('assistant.semantic.title')}</h3>
      <p className="semantic-context">
        {t(`assistant.semantic.${differentCandidate ? 'checkedCandidate' : 'checkedAnswer'}`)}
      </p>
      <Badge>{t(`assistant.semantic.status.${outcome.status}`)}</Badge>
      {outcome.status === 'failed' ? (
        <p>{t(`assistant.semantic.failure.${outcome.code}`)}</p>
      ) : null}
      {outcome.status === 'not_run' ? (
        <p>{t(`assistant.semantic.notRun.${outcome.reason}`)}</p>
      ) : null}
      {assessment ? (
        <>
          <dl className="semantic-assessment">
            <div>
              <dt>{t('assistant.semantic.claimSupport')}</dt>
              <dd>
                <Badge>{t(`assistant.semantic.support.${assessment.claimSupport}`)}</Badge>
              </dd>
            </div>
            <div>
              <dt>{t('assistant.semantic.completeness')}</dt>
              <dd>{t(`assistant.semantic.complete.${assessment.completeness}`)}</dd>
            </div>
            <div>
              <dt>{t('assistant.semantic.responseClassification')}</dt>
              <dd>{t(`assistant.semantic.response.${assessment.responseClassification}`)}</dd>
            </div>
          </dl>
          {assessment.missingCaveats.length > 0 ? (
            <div className="semantic-caveats">
              <h4>{t('assistant.semantic.missingCaveats')}</h4>
              {assessment.missingCaveats.map((caveat, index) => (
                <article key={index}>
                  <p className="semantic-exact-text" tabIndex={0}>
                    {caveat.description}
                  </p>
                  <SourceQuotes evidence={caveat.evidence} report={report} />
                </article>
              ))}
            </div>
          ) : null}
        </>
      ) : null}
      {report.answerUnits.length > 0 ? (
        <div className="semantic-units">
          <h4>{t('assistant.semantic.answerBlocks')}</h4>
          {report.answerUnits.map((unit, index) => {
            const verdict = assessment?.units.find((entry) => entry.unitId === unit.unitId)
            return (
              <details key={unit.unitId} open>
                <summary>
                  {t('assistant.semantic.block', { number: index + 1 })}
                  {verdict ? (
                    <Badge>{t(`assistant.semantic.support.${verdict.support}`)}</Badge>
                  ) : null}
                </summary>
                <p
                  className="semantic-exact-text"
                  tabIndex={0}
                  aria-label={t('assistant.semantic.checkedText')}
                >
                  {unit.text}
                </p>
                {verdict?.assertsDocumentAbsence ? (
                  <p>{t('assistant.semantic.assertsAbsence')}</p>
                ) : null}
                {verdict ? <SourceQuotes evidence={verdict.evidence} report={report} /> : null}
              </details>
            )
          })}
        </div>
      ) : null}
    </section>
  )
}
