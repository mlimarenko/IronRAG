import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import i18n from 'i18next'
import { MemoryRouter, useLocation } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { AssistantEvidenceView } from './model/assistantAdapter'
import { EvidenceDrawer } from './EvidenceDrawer'
import type { SemanticVerificationReport } from '@/shared/api/generated'

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({ locale: 'ru' }),
}))

const emptyEvidence: AssistantEvidenceView = {
  answerDisposition: 'non_terminal',
  chunkRefs: [],
  clarification: {
    answerCandidates: [],
    question: null,
    required: false,
  },
  entityRefs: [],
  factRefs: [],
  referenceSummary: null,
  relationRefs: [],
  runtimeSummary: {
    stages: [],
    totalEntities: 0,
    totalFacts: 0,
    totalRelations: 0,
    totalSegments: 0,
  },
  segmentRefs: [],
  verificationState: 'not_run',
  verificationWarnings: [],
}

function CurrentLocation() {
  const location = useLocation()
  return (
    <output aria-label="location">
      {location.pathname}
      {location.search}
    </output>
  )
}

function renderDrawer(evidence: AssistantEvidenceView) {
  return render(
    <MemoryRouter>
      <EvidenceDrawer evidence={evidence} open onOpenChange={vi.fn()} />
      <CurrentLocation />
    </MemoryRouter>,
  )
}

function semanticReport(): SemanticVerificationReport {
  return {
    protocolVersion: 1,
    questionSha256: 'c'.repeat(64),
    checkedAnswerSha256: 'a'.repeat(64),
    visibleAnswerSha256: 'a'.repeat(64),
    evidenceManifestSha256: 'b'.repeat(64),
    elapsedMs: 12,
    providerCallId: null,
    answerUnits: [{ unitId: 'unit-1', span: { start: 0, end: 18 }, text: 'Available to all.\n' }],
    sources: [
      {
        sourceId: 'source-1',
        documentId: 'document-1',
        documentRevisionId: 'revision-1',
        locator: { kind: 'chunk', chunk_id: 'chunk-1' },
        scope: 'selected_excerpt',
        textSha256: 'd'.repeat(64),
        text: 'Only administrators can enable it.',
      },
    ],
    outcome: {
      status: 'completed',
      assessment: {
        claimSupport: 'mixed',
        completeness: 'missing_essential_caveats',
        responseClassification: 'factual_answer',
        units: [
          {
            unitId: 'unit-1',
            support: 'contradicted',
            assertsDocumentAbsence: false,
            evidence: [
              {
                sourceId: 'source-1',
                span: { start: 0, end: 33 },
                quote: 'Only administrators can enable it.',
              },
            ],
          },
        ],
        missingCaveats: [
          {
            description: 'Administrator access is required.',
            evidence: [
              {
                sourceId: 'source-1',
                span: { start: 0, end: 33 },
                quote: 'Only administrators can enable it.',
              },
            ],
          },
        ],
      },
    },
  }
}

describe('EvidenceDrawer', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('ru')
    i18n.addResource(
      'ru',
      'translation',
      'assistant.runtimeStageLabels.extract_graph',
      'Извлечение графа',
    )
  })

  it('renders missing evidence as a stacked empty state', async () => {
    renderDrawer(emptyEvidence)

    const dialog = await screen.findByRole('dialog')
    const emptyState = dialog.querySelector('.state-card--stacked')

    expect(emptyState).not.toBeNull()
    expect(emptyState?.querySelector('.lucide-inbox')).not.toBeNull()
    expect(emptyState?.querySelector('.lucide-check')).toBeNull()
    expect(screen.queryByText('Оценка ответа')).not.toBeInTheDocument()
  })

  it('localizes runtime stages and technical fact kinds for the operator', async () => {
    renderDrawer({
      ...emptyEvidence,
      factRefs: [
        {
          confidence: 0.95,
          documentName: 'Reference',
          factKind: 'http_method',
          value: 'GET',
        },
      ],
      runtimeSummary: {
        ...emptyEvidence.runtimeSummary!,
        stages: [{ durationMs: 125, stage: 'extract_graph' }],
        totalFacts: 1,
      },
    })

    expect(await screen.findByText('Извлечение графа')).toBeInTheDocument()
    expect(screen.getByText('HTTP-метод')).toBeInTheDocument()
    expect(screen.queryByText('Extract graph')).not.toBeInTheDocument()
    expect(screen.queryByText('Http method')).not.toBeInTheDocument()
  })

  it('collapses the evidence summary to one column below the tablet breakpoint', async () => {
    renderDrawer({
      ...emptyEvidence,
      factRefs: [
        {
          confidence: 0.95,
          documentName: 'Reference',
          factKind: 'http_method',
          value: 'GET',
        },
      ],
      runtimeSummary: {
        ...emptyEvidence.runtimeSummary!,
        totalFacts: 1,
      },
    })

    const dialog = await screen.findByRole('dialog')
    expect(dialog.querySelector('.evidence-summary')).toHaveClass('max-md:grid-cols-1!')
  })

  it('separates support, caveats and response type and opens only the manifest document', async () => {
    const user = userEvent.setup()
    const report = semanticReport()
    renderDrawer({ ...emptyEvidence, semanticVerification: report })

    expect(await screen.findByText('Подтверждено частично')).toBeInTheDocument()
    expect(screen.getByText('Противоречит источникам')).toBeInTheDocument()
    expect(screen.getByText('Пропущены существенные оговорки')).toBeInTheDocument()
    expect(screen.getByText('Фактический ответ')).toBeInTheDocument()
    expect(screen.getByText('Administrator access is required.')).toBeInTheDocument()
    expect(screen.getAllByText('Only administrators can enable it.')).toHaveLength(2)
    expect(screen.getByLabelText('Точный проверенный текст').textContent).toBe(
      report.answerUnits[0]!.text,
    )
    expect(screen.queryByText(report.checkedAnswerSha256)).not.toBeInTheDocument()
    screen.getAllByRole('button', { name: 'Открыть исходный документ' })[0]!.focus()
    await user.keyboard('{Enter}')
    expect(screen.getByLabelText('location')).toHaveTextContent('/documents?documentId=document-1')
  })

  it.each([
    ['supported', 'Подтверждено источниками'],
    ['contradicted', 'Противоречит источникам'],
    ['insufficient_evidence', 'Недостаточно информации'],
    ['no_factual_claims', 'Нет фактических утверждений'],
  ] as const)(
    'renders the %s block verdict without deriving it from readiness',
    async (support, label) => {
      const report = semanticReport()
      if (report.outcome.status !== 'completed') throw new Error('Expected completed fixture')
      report.outcome.assessment.units[0]!.support = support
      renderDrawer({
        ...emptyEvidence,
        verificationState: 'verified',
        semanticVerification: report,
      })
      expect(await screen.findByText(label)).toBeInTheDocument()
    },
  )

  it.each([
    [
      'correct_source_bounded_abstention',
      'Обоснованное воздержание в пределах источника. Это не подтверждает недоступный факт.',
    ],
    ['false_refusal', 'Необоснованный отказ: на вопрос можно было ответить'],
    ['unresolved_refusal', 'Обоснованность отказа не установлена по доступным источникам'],
  ] as const)('renders %s independently from claimless support', async (classification, label) => {
    const report = semanticReport()
    if (report.outcome.status !== 'completed') throw new Error('Expected completed fixture')
    report.outcome.assessment.claimSupport = 'no_factual_claims'
    report.outcome.assessment.responseClassification = classification
    renderDrawer({ ...emptyEvidence, semanticVerification: report })
    expect(await screen.findByText(label)).toBeInTheDocument()
    expect(screen.getByText('Нет фактических утверждений')).toBeInTheDocument()
  })

  it.each([
    [
      { status: 'failed', code: 'timeout' },
      'Ошибка оценки',
      'Истекло время оценки. Оценка фактов не получена.',
    ],
    [
      { status: 'not_run', reason: 'no_evidence' },
      'Оценка не запускалась',
      'Для оценки нет доступного авторизованного текста источников.',
    ],
  ] as const)(
    'renders operational outcome %j without a factual verdict',
    async (outcome, label, reason) => {
      renderDrawer({ ...emptyEvidence, semanticVerification: { ...semanticReport(), outcome } })
      expect(await screen.findByText(label)).toBeInTheDocument()
      expect(screen.getByText(reason)).toBeInTheDocument()
      expect(screen.queryByText('Подтверждение фактов')).not.toBeInTheDocument()
      expect(screen.queryByText('Противоречит источникам')).not.toBeInTheDocument()
    },
  )

  it('labels findings about a rejected candidate instead of applying them to the visible fallback', async () => {
    renderDrawer({
      ...emptyEvidence,
      semanticVerification: { ...semanticReport(), visibleAnswerSha256: 'f'.repeat(64) },
    })
    expect(
      await screen.findByText(
        'Выводы относятся к проверенному кандидату, а не к заменившему его ответу в чате.',
      ),
    ).toBeInTheDocument()
    expect(screen.getByLabelText('Точный проверенный текст')).toHaveTextContent('Available to all.')
  })

  it('localizes the assessment in English and does not fabricate a link for an unresolved source', async () => {
    await i18n.changeLanguage('en')
    const report = semanticReport()
    report.sources = []
    renderDrawer({ ...emptyEvidence, semanticVerification: report })
    expect(await screen.findByText('Assessment completed')).toBeInTheDocument()
    expect(screen.getByText('Partially supported')).toBeInTheDocument()
    expect(screen.getByText('Contradicted by sources')).toBeInTheDocument()
    expect(screen.getByText('Essential caveats missing')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Open source document' })).not.toBeInTheDocument()
  })

  it('keeps citation pagination and the server total with a semantic report present', async () => {
    const user = userEvent.setup()
    renderDrawer({
      ...emptyEvidence,
      semanticVerification: semanticReport(),
      chunkRefs: Array.from({ length: 15 }, (_, index) => ({
        chunkId: `chunk-${index}`,
        executionId: 'execution-1',
        rank: index + 1,
        score: 0.5,
        sectionPath: [],
        documentTitle: `Citation ${index + 1}`,
      })),
      referenceSummary: { returnedCount: 15, totalCount: 27, truncated: true },
    })
    expect(await screen.findByText('Показано 12 из 27')).toBeInTheDocument()
    expect(screen.queryByText('Citation 13')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: i18n.t('assistant.showMore') }))
    expect(screen.getByText('Citation 15')).toBeInTheDocument()
    expect(screen.getByText('Показано 15 из 27')).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: i18n.t('assistant.showMore') }),
    ).not.toBeInTheDocument()
  })
})
