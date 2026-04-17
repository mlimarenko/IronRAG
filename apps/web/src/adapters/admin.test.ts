import { describe, expect, it } from 'vitest';

import { mapAudit } from './admin';

describe('mapAudit', () => {
  it('maps assistant call summaries from the audit payload', () => {
    const audit = mapAudit({
      id: 'evt-1',
      actionKind: 'query.execution.run',
      resultKind: 'succeeded',
      surfaceKind: 'mcp',
      createdAt: '2026-04-17T10:00:00Z',
      redactedMessage: 'assistant call completed',
      actorPrincipalId: 'principal-1',
      subjects: [{ subjectKind: 'query_execution', subjectId: 'exec-1' }],
      assistantCall: {
        queryExecutionId: 'exec-1',
        conversationId: 'conv-1',
        runtimeExecutionId: 'run-1',
        models: [{ providerKind: 'openai', modelName: 'gpt-5.4-mini' }],
        totalCost: '0.0123',
        currencyCode: 'USD',
        providerCallCount: 2,
      },
    });

    expect(audit.assistantCall).toEqual({
      queryExecutionId: 'exec-1',
      conversationId: 'conv-1',
      runtimeExecutionId: 'run-1',
      models: [{ providerKind: 'openai', modelName: 'gpt-5.4-mini' }],
      totalCost: '0.0123',
      currencyCode: 'USD',
      providerCallCount: 2,
    });
  });
});
