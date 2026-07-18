import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { LlmContextDebugResponse } from '@/shared/api/query'

import { AssistantDebugInspector } from './AssistantDebugInspector'

function renderInspector(snapshot: LlmContextDebugResponse) {
  return render(
    <AssistantDebugInspector
      t={i18n.t.bind(i18n)}
      open
      width={620}
      snapshot={snapshot}
      loading={false}
      error={null}
      evidence={null}
      onClose={vi.fn()}
      onWidthChange={vi.fn()}
    />,
  )
}

function baseSnapshot(overrides: Partial<LlmContextDebugResponse>): LlmContextDebugResponse {
  return {
    capturedAt: '2026-04-10T12:00:00Z',
    executionId: 'execution-alpha',
    finalAnswer: null,
    iterations: [],
    libraryId: 'library-alpha',
    question: 'What can Alpha Suite connect to?',
    totalIterations: 0,
    ...overrides,
  }
}

describe('AssistantDebugInspector', () => {
  it('renders the latest provider transcript plus the persisted assistant answer', () => {
    renderInspector(
      baseSnapshot({
        finalAnswer: 'Alpha Suite supports Provider Beta.',
        iterations: [
          {
            iteration: 1,
            providerKind: 'openai',
            modelName: 'gpt-test',
            requestMessages: [
              { role: 'system', content: 'system prompt' },
              { role: 'user', content: 'What can Alpha Suite connect to?' },
            ],
            responseText: null,
            responseToolCalls: [
              {
                id: 'call-stale',
                name: 'grounded_answer',
                argumentsJson: '{}',
                resultText: 'stale first result',
                resultJson: null,
                isError: false,
              },
            ],
            usage: {},
          },
          {
            iteration: 2,
            providerKind: 'openai',
            modelName: 'gpt-test',
            requestMessages: [
              { role: 'system', content: 'system prompt' },
              { role: 'user', content: 'What can Alpha Suite connect to?' },
              {
                role: 'assistant',
                content: null,
                tool_calls: [
                  {
                    id: 'call-alpha',
                    name: 'grounded_answer',
                    arguments_json: '{"query":"Alpha Suite"}',
                  },
                ],
              },
              {
                role: 'tool',
                name: 'grounded_answer',
                tool_call_id: 'call-alpha',
                content: 'tool result for Alpha Suite',
              },
            ],
            responseText: 'Alpha Suite supports Provider Beta.',
            responseToolCalls: [],
            usage: {},
          },
        ],
        totalIterations: 2,
      }),
    )

    fireEvent.click(screen.getByRole('button', { name: /Context/i }))

    expect(screen.getByText('tool result for Alpha Suite')).toBeInTheDocument()
    expect(screen.getByText('Alpha Suite supports Provider Beta.')).toBeInTheDocument()
    expect(screen.queryByText('stale first result')).not.toBeInTheDocument()
  })

  it('resizes the inspector with horizontal slider keys', () => {
    const onWidthChange = vi.fn()
    render(
      <AssistantDebugInspector
        t={i18n.t.bind(i18n)}
        open
        width={620}
        snapshot={null}
        loading={false}
        error={null}
        evidence={null}
        onClose={vi.fn()}
        onWidthChange={onWidthChange}
      />,
    )

    const slider = screen.getByRole('slider', { name: /resize/i })
    expect(slider).toHaveAttribute('type', 'range')
    expect(slider).toHaveAttribute('min', '420')
    expect(slider).toHaveAttribute('max', '960')

    fireEvent.keyDown(slider, { key: 'ArrowLeft' })
    expect(onWidthChange).toHaveBeenLastCalledWith(604)

    fireEvent.keyDown(slider, { key: 'ArrowRight' })
    expect(onWidthChange).toHaveBeenLastCalledWith(636)
  })

  it('labels every focusable pipeline segment with its stage, duration, and share', () => {
    const evidence = {
      segmentRefs: [],
      factRefs: [],
      entityRefs: [],
      relationRefs: [],
      verificationState: 'passed' as const,
      verificationWarnings: [],
      answerDisposition: 'factual_ready' as const,
      clarification: {
        required: false,
        question: null,
        answerCandidates: [],
      },
      runtimeSummary: {
        totalSegments: 0,
        totalFacts: 0,
        totalEntities: 0,
        totalRelations: 0,
        stages: [
          { stage: 'compile', durationMs: 25, itemCount: 1 },
          { stage: 'retrieve', durationMs: 75, itemCount: 2 },
        ],
        policyInterventions: [],
      },
    }

    render(
      <AssistantDebugInspector
        t={i18n.t.bind(i18n)}
        open
        width={620}
        snapshot={null}
        loading={false}
        error={null}
        evidence={evidence}
        onClose={vi.fn()}
        onWidthChange={vi.fn()}
      />,
    )

    expect(screen.getByRole('button', { name: /25 ms · 25%/ })).toHaveAccessibleName()
    expect(screen.getByRole('button', { name: /75 ms · 75%/ })).toHaveAccessibleName()
  })

  it('shows short-circuited tool results before the final answer', () => {
    renderInspector(
      baseSnapshot({
        finalAnswer: 'Alpha Suite has a documented Provider Beta integration.',
        iterations: [
          {
            iteration: 1,
            providerKind: 'openai',
            modelName: 'gpt-test',
            requestMessages: [
              { role: 'system', content: 'system prompt' },
              { role: 'user', content: 'List Alpha Suite integrations.' },
            ],
            responseText: null,
            responseToolCalls: [
              {
                id: 'call-alpha',
                name: 'grounded_answer',
                argumentsJson: '{"query":"Alpha Suite integrations"}',
                resultText: 'grounded tool result: Provider Beta',
                resultJson: { answer: 'Provider Beta' },
                isError: false,
              },
            ],
            usage: {},
          },
        ],
        totalIterations: 1,
      }),
    )

    fireEvent.click(screen.getByRole('button', { name: /Context/i }))

    expect(screen.getByText('grounded tool result: Provider Beta')).toBeInTheDocument()
    expect(
      screen.getByText('Alpha Suite has a documented Provider Beta integration.'),
    ).toBeInTheDocument()
  })
})
