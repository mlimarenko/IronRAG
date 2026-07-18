import { describe, expect, it, vi } from 'vitest'

vi.mock('./generated', () => ({
  Iam: {
    getBootstrapStatus: vi.fn(),
  },
}))

import { Iam } from './generated'
import { authApi } from './auth'
import type { ApiError } from './runtime'

describe('authApi', () => {
  it('preserves an Error message returned by the generated client', async () => {
    vi.mocked(Iam.getBootstrapStatus).mockResolvedValue({
      error: new Error('authentication service unavailable'),
    } as never)

    await expect(authApi.getBootstrapStatus()).rejects.toMatchObject({
      body: { error: 'authentication service unavailable' },
      message: 'authentication service unavailable',
      status: 0,
    } satisfies Partial<ApiError>)
  })
})
