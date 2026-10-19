import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useState } from 'react'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'

import { documentsApi, queries } from '@/shared/api'
import type { DocumentItem } from '@/shared/types'

type Section = 'pipeline' | 'revisions' | 'segments' | 'facts'

type Input = {
  activateListPollGrace: () => void
  document: DocumentItem | null
  errorMessage: (error: unknown, fallback: string) => string
  fetchSelectedDetail: (documentId: string) => Promise<void>
  loadFirstPage: () => Promise<void>
  onClose: () => void
  onDocumentHintUpdated: (documentId: string, hint: string | null) => void
  section: Section
  t: TFunction
}

const PAGE_LIMIT = 50

export function revisionFileName(fileName: string, revisionNumber: number): string {
  const lastDot = fileName.lastIndexOf('.')
  const suffix = ` (v${revisionNumber})`
  if (lastDot <= 0) return `${fileName}${suffix}`
  return `${fileName.slice(0, lastDot)}${suffix}${fileName.slice(lastDot)}`
}

export function useDocumentInspectorController(input: Input) {
  const queryClient = useQueryClient()
  const id = input.document?.id ?? ''
  const [segmentOffset, setSegmentOffset] = useState(0)
  const [factOffset, setFactOffset] = useState(0)
  const revisions = useQuery({
    ...queries.listContentRevisionsOptions({ path: { documentId: id } }),
    enabled: !!id && input.section === 'revisions',
  })
  const segments = useQuery({
    ...queries.listContentPreparedSegmentsOptions({
      path: { documentId: id },
      query: { offset: segmentOffset, limit: PAGE_LIMIT },
    }),
    enabled: !!id && input.section === 'segments',
  })
  const facts = useQuery({
    ...queries.listContentTechnicalFactsOptions({
      path: { documentId: id },
      query: { offset: factOffset, limit: PAGE_LIMIT },
    }),
    enabled: !!id && input.section === 'facts',
  })

  const refreshDocument = useCallback(async () => {
    if (!id) return
    await Promise.all([input.loadFirstPage(), input.fetchSelectedDetail(id)])
  }, [id, input])

  const saveHint = useCallback(
    async (hint: string) => {
      if (!id) return false
      try {
        const saved = await documentsApi.updateDocumentHint(id, hint.trim() || null)
        input.onDocumentHintUpdated(id, saved)
        toast.success(input.t('documents.documentHintUpdated'))
        return true
      } catch (error) {
        toast.error(input.errorMessage(error, input.t('documents.documentHintUpdateFailed')))
        return false
      }
    },
    [id, input],
  )

  const retry = useCallback(async () => {
    if (!id) return
    try {
      await documentsApi.reprocess(id)
      input.activateListPollGrace()
      await refreshDocument()
    } catch (error) {
      toast.error(input.errorMessage(error, input.t('documents.reprocessFailed')))
    }
  }, [id, input, refreshDocument])

  const replace = useCallback(
    async (file: File) => {
      if (!id) return false
      try {
        await documentsApi.replace(id, file)
        input.activateListPollGrace()
        await queryClient.invalidateQueries({
          queryKey: queries.listContentRevisionsOptions({ path: { documentId: id } }).queryKey,
        })
        await refreshDocument()
        toast.success(input.t('documents.replaceFileSuccess'))
        return true
      } catch (error) {
        toast.error(input.errorMessage(error, input.t('documents.replaceFileFailed')))
        return false
      }
    },
    [id, input, queryClient, refreshDocument],
  )

  const remove = useCallback(async () => {
    if (!id) return
    try {
      await documentsApi.delete(id)
      input.onClose()
      await input.loadFirstPage()
    } catch (error) {
      toast.error(input.errorMessage(error, input.t('documents.deleteFailed')))
    }
  }, [id, input])

  const download = useCallback(
    async (revision?: { id: string; revisionNumber: number }) => {
      if (!id || !input.document) return
      try {
        const blob = await documentsApi.getSourceBlob(id, revision?.id)
        const href = URL.createObjectURL(blob)
        const anchor = window.document.createElement('a')
        anchor.href = href
        // An absent publication name must not be reconstructed from identity.
        // Empty download attribute delegates the saved name to the browser.
        anchor.setAttribute('download', '')
        if (input.document.fileName !== null) {
          anchor.download = revision
            ? revisionFileName(input.document.fileName, revision.revisionNumber)
            : input.document.fileName
        }
        anchor.click()
        URL.revokeObjectURL(href)
      } catch {
        const fallback = revision
          ? input.t('documents.revisionDownloadFailed')
          : input.t('documents.failedToLoad')
        toast.error(fallback)
      }
    },
    [id, input],
  )

  return {
    download,
    errorMessage: input.errorMessage,
    factOffset,
    facts,
    PAGE_LIMIT,
    refreshDocument,
    remove,
    replace,
    retry,
    revisions,
    saveHint,
    segmentOffset,
    segments,
    setFactOffset,
    setSegmentOffset,
  }
}

export type DocumentInspectorController = ReturnType<typeof useDocumentInspectorController>
