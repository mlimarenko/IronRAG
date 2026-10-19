import { render, screen } from '@testing-library/react'
import i18n from 'i18next'
import { MemoryRouter } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { AssistantEvidenceView } from './model/assistantAdapter'
import { EvidenceDrawer } from './EvidenceDrawer'

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

function renderDrawer(evidence: AssistantEvidenceView) {
  return render(
    <MemoryRouter>
      <EvidenceDrawer evidence={evidence} open onOpenChange={vi.fn()} />
    </MemoryRouter>,
  )
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
})
