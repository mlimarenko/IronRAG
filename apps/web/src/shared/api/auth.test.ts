import { describe, expect, it, vi } from 'vitest'

vi.mock('./generated', () => ({
  Iam: {
    getIamBootstrap: vi.fn(),
    getIamContext: vi.fn(),
    createIamSession: vi.fn(),
    deleteIamSession: vi.fn(),
    createIamBootstrap: vi.fn(),
  },
}))

import { Iam } from './generated'
import { authApi } from './auth'
import type { ApiError } from './runtime'

describe('authApi', () => {
  it('normalizes an Error returned by the generated client to RFC9457 detail', async () => {
    vi.mocked(Iam.getIamBootstrap).mockResolvedValue({
      data: undefined,
      error: new Error('authentication service unavailable'),
    })

    await expect(authApi.getIamBootstrap()).rejects.toMatchObject({
      body: { detail: 'authentication service unavailable', status: 0 },
      message: 'authentication service unavailable',
      status: 0,
    } satisfies Partial<ApiError>)
  })

  it('rejects an empty successful session response with the operation name', async () => {
    vi.mocked(Iam.getIamContext).mockResolvedValue({ data: undefined, error: undefined })

    await expect(authApi.resolveSession()).rejects.toThrow(
      'resolveSession returned no response body',
    )
  })

  it('sends login credentials only in the login session request body', async () => {
    vi.mocked(Iam.createIamSession).mockResolvedValue({ data: { id: 'session-1' } } as never)

    await expect(authApi.login('member@example.test', 'correct horse')).resolves.toEqual({
      id: 'session-1',
    })
    expect(Iam.createIamSession).toHaveBeenCalledWith({
      body: { login: 'member@example.test', password: 'correct horse' },
    })
  })

  it('unwraps bootstrap setup and logout responses through generated IAM operations', async () => {
    vi.mocked(Iam.createIamBootstrap).mockResolvedValue({ data: { id: 'session-2' } } as never)
    vi.mocked(Iam.deleteIamSession).mockResolvedValue({ data: undefined } as never)

    await expect(
      authApi.bootstrapSetup({ login: 'admin@example.test', password: 'correct horse' } as never),
    ).resolves.toEqual({ id: 'session-2' })
    await expect(authApi.logout()).resolves.toBeUndefined()
  })
})
