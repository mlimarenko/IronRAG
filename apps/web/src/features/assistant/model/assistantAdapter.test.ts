import { describe, expect, it } from 'vitest'

import type { AssistantConversationMessage, AssistantEvidenceBundle } from '@/shared/api/generated'

import { mapAssistantMessage, mapAssistantTurnToEvidence } from './assistantAdapter'

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

  it('carries a chunk citation as something a reader can open, not just a rank', () => {
    const transport = {
      answerDisposition: 'factual_ready',
      clarification: { required: false, answerCandidates: [] },
      chunkReferences: [
        {
          chunkId: 'chunk-1',
          executionId: 'execution-1',
          rank: 2,
          score: 0.87,
          documentId: 'document-1',
          documentTitle: 'Alpha Suite handbook',
          sectionPath: ['Setup', 'Prerequisites'],
          excerpt: 'The runtime requires two accounts.',
        },
      ],
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      referenceSummary: { returnedCount: 1, totalCount: 14, truncated: true },
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeSummary: {},
      runtimeStageSummaries: [],
    } as unknown as AssistantEvidenceBundle

    const mapped = mapAssistantTurnToEvidence(transport)

    expect(mapped.chunkRefs).toEqual([
      {
        chunkId: 'chunk-1',
        executionId: 'execution-1',
        rank: 2,
        score: 0.87,
        documentId: 'document-1',
        documentTitle: 'Alpha Suite handbook',
        sectionPath: ['Setup', 'Prerequisites'],
        excerpt: 'The runtime requires two accounts.',
      },
    ])
    expect(mapped.referenceSummary).toEqual({
      returnedCount: 1,
      totalCount: 14,
      truncated: true,
    })
  })

  it('leaves a chunk whose document is gone with a rank and nothing invented', () => {
    // The detail is read back from the chunk rows at display time, so a
    // document deleted since the answer simply has none. An empty string in
    // place of a title would render as a blank source rather than as one the
    // panel knows nothing about.
    const transport = {
      answerDisposition: 'factual_ready',
      clarification: { required: false, answerCandidates: [] },
      chunkReferences: [{ chunkId: 'chunk-2', executionId: 'execution-1', rank: 1, score: 0.4 }],
      preparedSegmentReferences: [],
      technicalFactReferences: [],
      entityReferences: [],
      relationReferences: [],
      referenceSummary: null,
      verificationState: 'verified',
      verificationWarnings: [],
      runtimeSummary: {},
      runtimeStageSummaries: [],
    } as unknown as AssistantEvidenceBundle

    const mapped = mapAssistantTurnToEvidence(transport)

    expect(mapped.chunkRefs).toEqual([
      { chunkId: 'chunk-2', executionId: 'execution-1', rank: 1, score: 0.4, sectionPath: [] },
    ])
  })

  it('hydrates answer duration from the server runtime clock only', () => {
    const message = {
      id: 'message-1',
      role: 'assistant',
      content: 'Grounded answer',
      timestamp: '2026-01-01T00:00:30Z',
      executionId: 'execution-1',
      evidence: {
        answerDisposition: 'factual_ready',
        clarification: { required: false, answerCandidates: [] },
        chunkReferences: [],
        preparedSegmentReferences: [],
        technicalFactReferences: [],
        entityReferences: [],
        relationReferences: [],
        verificationState: 'verified',
        verificationWarnings: [],
        runtimeSummary: {
          acceptedAt: '2026-01-01T00:00:10Z',
          completedAt: '2026-01-01T00:00:25Z',
        },
        runtimeStageSummaries: [],
      },
    } as unknown as AssistantConversationMessage

    expect(mapAssistantMessage(message).durationMs).toBe(15_000)
  })
})
