import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

import { useDocumentsStore } from './documents'

describe('documents store workspace ordering', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('keeps degraded notices ahead of informational notices and preserves primary summary', () => {
    const store = useDocumentsStore()

    store.workspaceSummary = {
      primarySummary: {
        progressLabel: '193 / 280',
        spendLabel: '$3.50 settled',
        backlogLabel: '76 remaining',
        terminalState: 'failed_with_residual_work',
      },
      secondaryDiagnostics: [],
      degradedNotices: [
        {
          kind: 'projection_contention',
          title: 'Projection contention',
          message: 'Projection retries are active.',
        },
      ],
      informationalNotices: [
        {
          kind: 'backlog',
          title: 'Backlog',
          message: 'Healthy queue remains active.',
        },
      ],
      tableDocumentCount: 280,
      activeFilterCount: 0,
      highlightedStatus: 'failed',
    }
    store.terminalOutcomeSnapshot = {
      terminalState: 'failed_with_residual_work',
      residualReason: 'provider_failure',
      queuedCount: 0,
      processingCount: 2,
      pendingGraphCount: 1,
      failedDocumentCount: 3,
      settledAt: null,
      lastTransitionAt: '2026-03-19T10:00:00Z',
    }
    store.providerFailureDetail = {
      failureClass: 'upstream_timeout',
      providerKind: 'openai',
      modelName: 'gpt-5.4-mini',
      requestShapeKey: 'graph_extract_v3:initial:segments_3:trimmed',
      requestSizeBytes: 32000,
      upstreamStatus: '504',
      elapsedMs: 45000,
      retryDecision: 'retrying_provider_call',
      usageVisible: true,
    }

    expect(store.workspacePrimarySummary?.progressLabel).toBe('193 / 280')
    expect(store.workspaceNoticeGroups.degraded.length).toBeGreaterThan(0)
    expect(store.workspaceNoticeGroups.degraded[0]?.kind).toBe('projection_contention')
    expect(
      store.workspaceNoticeGroups.degraded.some((notice) => notice.kind === 'provider_failure_count'),
    ).toBe(true)
    expect(
      store.workspaceNoticeGroups.degraded.some((notice) =>
        notice.kind.startsWith('selected_provider_failure:'),
      ),
    ).toBe(true)
    expect(store.workspaceNoticeGroups.informational).toEqual([
      {
        kind: 'backlog',
        title: 'Backlog',
        message: 'Healthy queue remains active.',
      },
    ])
  })
})
