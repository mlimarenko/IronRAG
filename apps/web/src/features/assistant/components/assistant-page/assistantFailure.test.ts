import { describe, expect, it } from 'vitest'
import type { TFunction } from 'i18next'

import { ApiError } from '@/shared/api/runtime'
import { AssistantTurnFailedEventError } from '@/shared/api/query'
import { classifyAssistantFailure } from './useAssistantSession'

const t = ((key: string) => key) as unknown as TFunction

describe('classifyAssistantFailure', () => {
  it('says what happened when the turn fails mid-stream', () => {
    // Every failure the assistant screen produces arrives this way, as a stream
    // event rather than as a problem response. Reading only the problem body
    // sent all of them to the generic apology, whose advice — try again — is
    // wrong for a library that is still indexing.
    const failure = classifyAssistantFailure(
      new AssistantTurnFailedEventError(
        'conflict: library content is still being indexed',
        'knowledge_not_ready',
      ),
      t,
    )

    expect(failure.inlineMessage).toBe('assistant.libraryContentUnavailableInline')
    expect(failure.unusableLibrary).toBe(true)
    expect(failure.retryDiagnosis).toBeNull()
  })

  it('still apologises for a reason it has no words for', () => {
    const failure = classifyAssistantFailure(
      new AssistantTurnFailedEventError('assistant turn stream failed unexpectedly', 'internal'),
      t,
    )

    expect(failure.inlineMessage).toBe('assistant.serverFailure')
    expect(failure.unusableLibrary).toBe(false)
  })

  it('reads the same code from a problem response', () => {
    const failure = classifyAssistantFailure(
      new ApiError(409, {
        type: 'about:blank',
        title: 'Knowledge Not Ready',
        status: 409,
        detail: 'library content is still being indexed',
        code: 'knowledge_not_ready',
      }),
      t,
    )

    expect(failure.inlineMessage).toBe('assistant.libraryContentUnavailableInline')
  })
})
