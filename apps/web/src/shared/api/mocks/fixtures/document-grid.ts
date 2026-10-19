import type { ContentDocumentListItem } from '../../generated'

export default function documentGridItems(): ContentDocumentListItem[] {
  return Array.from({ length: 31 }, (_, index) => ({
    cost: index === 30 ? '99.125' : `${index}.10`,
    costCurrencyCode: 'USD',
    documentState: 'active',
    externalKey: `grid-document-${index}`,
    fileName:
      index === 30 ? 'Needle document.pdf' : `Row document ${String(index).padStart(2, '0')}.txt`,
    fileSize: index === 30 ? 1537 : 1024 + index,
    fileType: index % 2 ? 'text/plain' : 'application/pdf',
    id: `019f8aad-86a5-7183-bb73-${String(index).padStart(12, '0')}`,
    libraryId: 'library-demo-1',
    processingStartedAt: index === 30 ? '2037-04-02T11:58:29.875Z' : '2026-09-01T10:00:00.000Z',
    processingFinishedAt: index === 30 ? '2037-04-02T12:00:00.000Z' : '2026-09-01T10:00:02.000Z',
    readiness: index % 2 ? 'graph_sparse' : 'graph_ready',
    retryable: false,
    status: index % 2 ? 'failed' : 'ready',
    uploadedAt: index === 30 ? '1999-04-02T12:00:00.000Z' : '2026-08-20T09:00:00.000Z',
    workspaceId: 'workspace-alpha',
  }))
}
