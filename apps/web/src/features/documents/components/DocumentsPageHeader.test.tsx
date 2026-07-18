import { createRef } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { act } from 'react'
import { afterEach, describe, expect, it } from 'vitest'

import { DocumentsPageHeader } from './DocumentsPageHeader'

describe('DocumentsPageHeader', () => {
  let container: HTMLDivElement | null = null
  let root: Root | null = null

  afterEach(async () => {
    if (root) {
      await act(async () => root?.unmount())
    }
    container?.remove()
    container = null
    root = null
  })

  it('does not expose the admin ingest queue as a documents tab', async () => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    const fileInputRef = createRef<HTMLInputElement>()
    const folderInputRef = createRef<HTMLInputElement>()
    const labels: Record<string, string> = {
      'documents.title': 'Documents',
      'documents.subtitle': 'Manage ingestion and monitor processing',
      'documents.tabs.documents': 'Documents',
      'documents.tabs.webIngest': 'Web Ingest',
      'documents.activeWebRun': 'Active web ingest run',
      'documents.addContent': 'Add content',
      'documents.addContentFiles': 'Files',
      'documents.addContentFolder': 'Folder',
      'documents.refreshRuns': 'Refresh',
      'documents.addLink': 'Add link',
    }
    const t = ((key: string) => labels[key] ?? key) as never

    await act(async () => {
      root?.render(
        <DocumentsPageHeader
          activeTab="documents"
          canUpload
          documentsCount={2}
          fileInputRef={fileInputRef}
          folderInputRef={folderInputRef}
          handleFileSelect={() => undefined}
          handleFolderSelect={() => undefined}
          hasActiveWebRun={false}
          setActiveTab={() => undefined}
          setAddLinkOpen={() => undefined}
          setBoundaryPolicy={() => undefined}
          setCrawlMode={() => undefined}
          setMaxDepth={() => undefined}
          setMaxPages={() => undefined}
          setSeedUrl={() => undefined}
          onRefreshWebRuns={() => undefined}
          t={t}
          webRunsRefreshing={false}
          webRunsCount={0}
          ingestionReady
          onOpenAiSettings={() => undefined}
        />,
      )
    })

    expect(container.textContent).toContain('Documents')
    expect(container.textContent).toContain('Web Ingest')
    expect(container.textContent).not.toContain('Queue')
  })
})
