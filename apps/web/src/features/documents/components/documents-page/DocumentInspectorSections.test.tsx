import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DocumentLifecycleDetail } from '@/shared/api'
import i18n from '@/shared/i18n'

import {
  FactsSection,
  PipelineSection,
  RevisionsSection,
  SegmentsSection,
} from './DocumentInspectorSections'
import type { DocumentInspectorController } from './useDocumentInspectorController'

function buildController(
  overrides: Partial<DocumentInspectorController> = {},
): DocumentInspectorController {
  return {
    download: async () => undefined,
    errorMessage: (_error: unknown, fallback: string) => fallback,
    factOffset: 0,
    facts: {
      data: {
        documentId: 'document-1',
        items: [],
        limit: 50,
        offset: 0,
        total: 0,
      },
      error: null,
      isLoading: false,
    },
    PAGE_LIMIT: 50,
    refreshDocument: async () => undefined,
    remove: async () => undefined,
    replace: async () => false,
    retry: async () => undefined,
    revisions: {
      data: [],
      error: null,
      isLoading: false,
    },
    saveHint: async () => false,
    segmentOffset: 0,
    segments: {
      data: {
        documentId: 'document-1',
        items: [],
        limit: 50,
        offset: 0,
        total: 0,
      },
      error: null,
      isLoading: false,
    },
    setFactOffset: () => undefined,
    setSegmentOffset: () => undefined,
    ...overrides,
  } as unknown as DocumentInspectorController
}

describe('DocumentInspectorSections', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('humanizes provider identifiers in the pipeline timeline', () => {
    const lifecycle: DocumentLifecycleDetail = {
      attempts: [
        {
          attemptKind: 'initial',
          attemptNo: 1,
          jobId: 'job-1',
          queueStartedAt: '2026-08-10T10:00:00Z',
          stageEvents: [
            {
              providerKind: 'openai_compatible',
              stage: 'extract_graph',
              startedAt: '2026-08-10T10:00:01Z',
              status: 'completed',
            },
          ],
          status: 'completed',
        },
      ],
    }

    render(<PipelineSection lifecycle={lifecycle} locale="en" />)

    expect(screen.getByText('Openai compatible')).toBeInTheDocument()
    expect(screen.queryByText('openai_compatible')).not.toBeInTheDocument()
  })

  it('humanizes technical fact qualifier keys', () => {
    const controller = buildController({
      facts: {
        data: {
          documentId: 'document-1',
          items: [
            {
              canonicalValue: { value: 'GET', value_type: 'text' },
              confidence: 0.98,
              createdAt: '2026-08-10T10:00:00Z',
              displayValue: 'GET',
              documentId: 'document-1',
              extractionKind: 'native',
              factId: 'fact-1',
              factKind: 'http_method',
              libraryId: 'library-1',
              qualifiers: [{ key: 'request_method', value: 'GET' }],
              revisionId: 'revision-1',
              supportBlockIds: [],
              supportChunkIds: [],
              workspaceId: 'workspace-1',
            },
          ],
          limit: 50,
          offset: 0,
          total: 1,
        },
        error: null,
        isLoading: false,
      } as unknown as DocumentInspectorController['facts'],
    })

    render(<FactsSection controller={controller} />)

    expect(screen.getByText('Request method: GET')).toBeInTheDocument()
    expect(screen.queryByText(/request_method/)).not.toBeInTheDocument()
  })

  it('downloads the specific revision selected in its row, not the current document', async () => {
    const user = userEvent.setup()
    const download = vi.fn(async () => undefined)
    const controller = buildController({
      download,
      revisions: {
        data: [
          {
            byte_size: 10,
            checksum: 'sha256:new',
            content_source_kind: 'upload',
            created_at: '2026-08-20T10:00:00Z',
            document_id: 'document-1',
            id: 'revision-2',
            library_id: 'library-1',
            mime_type: 'text/plain',
            revision_number: 2,
          },
          {
            byte_size: 8,
            checksum: 'sha256:old',
            content_source_kind: 'upload',
            created_at: '2026-08-10T10:00:00Z',
            document_id: 'document-1',
            id: 'revision-1',
            library_id: 'library-1',
            mime_type: 'text/plain',
            revision_number: 1,
          },
        ],
        error: null,
        isLoading: false,
      } as unknown as DocumentInspectorController['revisions'],
    })

    render(<RevisionsSection controller={controller} locale="en" />)

    await user.click(screen.getByRole('button', { name: 'Download revision 1' }))

    expect(download).toHaveBeenCalledExactlyOnceWith({ id: 'revision-1', revisionNumber: 1 })
    expect(download).not.toHaveBeenCalledWith({ id: 'revision-2', revisionNumber: 2 })
    expect(download).not.toHaveBeenCalledWith()
  })

  it.each([
    [
      'revisions',
      (controller: DocumentInspectorController) => (
        <RevisionsSection controller={controller} locale="en" />
      ),
    ],
    [
      'segments',
      (controller: DocumentInspectorController) => <SegmentsSection controller={controller} />,
    ],
    [
      'facts',
      (controller: DocumentInspectorController) => <FactsSection controller={controller} />,
    ],
  ])('renders a centered neutral empty state for %s', (_section, renderSection) => {
    const { container } = render(renderSection(buildController()))

    expect(screen.getByText('Nothing here yet')).toBeInTheDocument()
    expect(container.querySelector('.state-card--stacked .lucide-inbox')).toBeInTheDocument()
    expect(container.querySelector('.lucide-circle-check, .lucide-check')).not.toBeInTheDocument()
  })
})
