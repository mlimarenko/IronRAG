import { describe, expect, it } from 'vitest'

import type { AssistantEvidenceBundle } from '@/shared/api/generated'

import { mapAssistantTurnToEvidence } from './assistantAdapter'

describe('mapAssistantTurnToEvidence', () => {
  it('preserves typed clarification disposition and candidates after history hydration', () => {
    const transport = {
      answerDisposition: 'clarification',
      clarification: {
        required: true,
        question: 'Which neutral variant?',
        answerCandidates: [
          {
            label: 'Neutral variant',
            kind: 'document',
            confidence: 0.75,
            provenance: {
              documentId: '00000000-0000-0000-0000-000000000001',
            },
          },
        ],
      },
      chunkReferences: [],
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      verificationState: 'not_run',
      verificationWarnings: [],
      runtimeSummary: {
        runtimeExecutionId: '00000000-0000-0000-0000-000000000002',
        lifecycleState: 'completed',
        activeStage: null,
        turnBudget: 1,
        turnCount: 1,
        parallelActionLimit: 1,
        failureCode: null,
        failureSummaryRedacted: null,
        policySummary: {
          allowCount: 0,
          rejectCount: 0,
          terminateCount: 0,
          recentDecisions: [],
        },
        acceptedAt: '2026-01-01T00:00:00Z',
        completedAt: '2026-01-01T00:00:01Z',
      },
      runtimeStageSummaries: [],
    } as unknown as AssistantEvidenceBundle

    const mapped = mapAssistantTurnToEvidence(transport)

    expect(mapped.answerDisposition).toBe('clarification')
    expect(mapped.clarification).toEqual({
      required: true,
      question: 'Which neutral variant?',
      answerCandidates: [
        {
          label: 'Neutral variant',
          kind: 'document',
          confidence: 0.75,
          provenance: {
            entityId: null,
            documentId: '00000000-0000-0000-0000-000000000001',
            chunkId: null,
          },
        },
      ],
    })
  })
})
