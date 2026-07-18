import { expect, test } from '@playwright/test'

import { installBrowserMocks, mockPath } from './support/mocks'

const WORKSPACE_ID = 'workspace-alpha'
const LIBRARY_ID = 'library-demo-1'
const SESSION_ID = 'session-source-links'

test('renders assistant evidence source access as a visible link', async ({ page }, testInfo) => {
  await installBrowserMocks(page, {
    querySessions: [
      {
        conversationState: 'active',
        createdAt: '2026-05-13T00:00:00.000Z',
        id: SESSION_ID,
        libraryId: LIBRARY_ID,
        title: 'Source answer',
        turnCount: 2,
        updatedAt: '2026-05-13T00:00:01.000Z',
        workspaceId: WORKSPACE_ID,
      },
    ],
    queryConversations: {
      [SESSION_ID]: {
        session: {
          conversationState: 'active',
          createdAt: '2026-05-13T00:00:00.000Z',
          id: SESSION_ID,
          libraryId: LIBRARY_ID,
          title: 'Source answer',
          turnCount: 2,
          updatedAt: '2026-05-13T00:00:01.000Z',
          workspaceId: WORKSPACE_ID,
        },
        messages: [
          {
            content: 'Where is the source?',
            id: 'turn-user',
            role: 'user',
            timestamp: '2026-05-13T00:00:00.000Z',
          },
          {
            content: 'The answer cites a source.',
            executionId: 'execution-source-link',
            id: 'turn-assistant',
            role: 'assistant',
            timestamp: '2026-05-13T00:00:01.000Z',
            evidence: {
              preparedSegmentReferences: [
                {
                  blockKind: 'heading',
                  documentId: 'doc-1',
                  documentTitle: 'Alpha Guide',
                  headingTrail: ['Alpha Guide'],
                  rank: 1,
                  score: 0.91,
                  sectionPath: [],
                  segmentId: 'seg-1',
                  sourceAccess: {
                    href: '/v1/content/documents/doc-1/source',
                    kind: 'stored_document',
                  },
                  sourceUri: 'upload://doc-1',
                },
              ],
              technicalFactReferences: [],
              entityReferences: [],
              relationReferences: [],
              verificationState: 'verified',
              verificationWarnings: [],
              runtimeStageSummaries: [],
            },
          },
        ],
      },
    },
  })

  await page.goto(mockPath('/assistant'))

  await page.getByRole('button', { name: 'Expand sessions' }).click()
  const sourceSession = page.getByRole('button', { name: /Source answer/ })
  await expect(sourceSession).toBeVisible()
  await sourceSession.click()
  await page.getByRole('button', { name: 'View evidence' }).click()

  const evidencePanel = page.getByTestId('assistant-evidence-scroll')
  await expect(evidencePanel.getByTitle('Alpha Guide')).toBeVisible()
  const sourceLink = evidencePanel.getByRole('link', { name: 'Open source document' })
  await expect(sourceLink).toBeVisible()
  await expect(sourceLink).toHaveAttribute('href', /\/v1\/content\/documents\/doc-1\/source$/)
  await expect(sourceLink).toHaveAttribute('target', '_blank')
  await sourceLink.hover()
  await expect(sourceLink).toHaveCSS('text-decoration-line', 'underline')

  await page.screenshot({
    path: testInfo.outputPath('assistant-source-links.png'),
    fullPage: false,
  })
})
