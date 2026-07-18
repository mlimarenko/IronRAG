import { describe, expect, it } from 'vitest'

import { createBrowserMockHandlers } from './e2e'
import { server } from './server'

describe('assistant session browser mocks', () => {
  it('persists rename and delete mutations in the mocked session list', async () => {
    server.use(
      ...createBrowserMockHandlers({
        authenticated: true,
        querySessions: [
          {
            conversationState: 'active',
            createdAt: '2026-04-10T10:00:00Z',
            id: 'session-1',
            libraryId: 'library-1',
            title: 'Original title',
            turnCount: 2,
            updatedAt: '2026-04-10T10:00:00Z',
            workspaceId: 'workspace-1',
          },
        ],
      }),
    )

    const initialList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(initialList.json()).resolves.toEqual([
      expect.objectContaining({ id: 'session-1', title: 'Original title' }),
    ])

    const renameResponse = await fetch('http://localhost:3000/v1/query/sessions/session-1', {
      body: JSON.stringify({ title: '  Durable   title  ' }),
      headers: { 'content-type': 'application/json' },
      method: 'PATCH',
    })
    expect(renameResponse.status).toBe(200)
    await expect(renameResponse.json()).resolves.toMatchObject({
      id: 'session-1',
      title: 'Durable title',
    })

    const renamedList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(renamedList.json()).resolves.toEqual([
      expect.objectContaining({ id: 'session-1', title: 'Durable title' }),
    ])

    const deleteResponse = await fetch('http://localhost:3000/v1/query/sessions/session-1', {
      method: 'DELETE',
    })
    expect(deleteResponse.status).toBe(204)

    const deletedList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(deletedList.json()).resolves.toEqual([])
  })
})
