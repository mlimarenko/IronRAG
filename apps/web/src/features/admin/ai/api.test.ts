import { describe, expect, it } from 'vitest'

import { isOperation } from './api'
import { scopeBody } from './utils'

describe('AI admin query invalidation', () => {
  it('matches only the exact generated operation id', () => {
    const matchesAccounts = isOperation('listAiAccounts')

    expect(
      matchesAccounts({ queryKey: [{ _id: 'listAiAccounts', query: { scopeKind: 'library' } }] }),
    ).toBe(true)
    expect(
      matchesAccounts({ queryKey: [{ _id: 'getAiAccount', path: { accountId: 'account-1' } }] }),
    ).toBe(false)
    expect(matchesAccounts({ queryKey: ['listAiAccounts'] })).toBe(false)
  })
})

describe('AI admin scope requests', () => {
  it('uses the same scope shape for instance, workspace, and library writes', () => {
    const context = { workspaceId: 'workspace-1', libraryId: 'library-1' }

    expect(scopeBody('instance', context)).toEqual({ scopeKind: 'instance' })
    expect(scopeBody('workspace', context)).toEqual({
      scopeKind: 'workspace',
      workspaceId: 'workspace-1',
    })
    expect(scopeBody('library', context)).toEqual({
      scopeKind: 'library',
      workspaceId: 'workspace-1',
      libraryId: 'library-1',
    })
  })
})
