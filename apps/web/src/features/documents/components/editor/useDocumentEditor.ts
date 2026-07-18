import { useCallback, useState } from 'react'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'

import { documentsApi } from '@/shared/api'
import type { DocumentItem } from '@/shared/types'

import { serializeSourceTextForEditor } from './documentEditorBlocks'
import { isCodeLikeSourceFormat, isPlainTextSourceFormat } from './editorSurfaceMode'

type EditorAvailability = {
  enabled: boolean
  readOnly: boolean
  reason: string | null
}

type UseDocumentEditorOptions = {
  editorAvailability: (doc: DocumentItem | null) => EditorAvailability
  errorMessage: (error: unknown, fallback: string) => string
  onDocumentSaved: (documentId: string) => Promise<void>
  onDocumentSelected: (doc: DocumentItem) => void | Promise<void>
  selectedDocumentId: string | null
  t: TFunction
}

export function useDocumentEditor({
  editorAvailability,
  errorMessage,
  onDocumentSaved,
  onDocumentSelected,
  selectedDocumentId,
  t,
}: UseDocumentEditorOptions) {
  const [editorOpen, setEditorOpen] = useState(false)
  const [editorLoading, setEditorLoading] = useState(false)
  const [editorSaving, setEditorSaving] = useState(false)
  const [editorMarkdown, setEditorMarkdown] = useState('')
  const [editorError, setEditorError] = useState<string | null>(null)
  const [editorDocument, setEditorDocument] = useState<DocumentItem | null>(null)
  const [editorReadOnly, setEditorReadOnly] = useState(false)

  const resetEditor = useCallback(() => {
    setEditorOpen(false)
    setEditorDocument(null)
    setEditorMarkdown('')
    setEditorError(null)
    setEditorLoading(false)
    setEditorSaving(false)
    setEditorReadOnly(false)
  }, [])

  const handleEditorOpenChange = useCallback(
    (open: boolean) => {
      if (open) {
        setEditorOpen(true)
        return
      }
      resetEditor()
    },
    [resetEditor],
  )

  const openEditor = useCallback(
    async (doc: DocumentItem) => {
      const availability = editorAvailability(doc)
      if (!availability.enabled) {
        toast.error(availability.reason ?? t('documents.editUnavailableGeneric'))
        return
      }

      if (selectedDocumentId !== doc.id) {
        void onDocumentSelected(doc)
      }

      setEditorDocument(doc)
      setEditorMarkdown('')
      setEditorError(null)
      setEditorLoading(true)
      setEditorReadOnly(availability.readOnly)
      setEditorOpen(true)

      try {
        const nextMarkdown = shouldLoadStoredSourceTextForEditor(doc)
          ? await loadSourceEditorMarkdown(doc)
          : await documentsApi.getEditorSourceText(doc.id)
        setEditorMarkdown(nextMarkdown)
      } catch (err: unknown) {
        setEditorError(errorMessage(err, t('documents.editor.loadFailed')))
      } finally {
        setEditorLoading(false)
      }
    },
    [editorAvailability, errorMessage, onDocumentSelected, selectedDocumentId, t],
  )

  const saveEditor = useCallback(
    async (markdown: string) => {
      if (!editorDocument) {
        return
      }

      const availability = editorAvailability(editorDocument)
      if (availability.readOnly) {
        return
      }

      const documentId = editorDocument.id
      setEditorSaving(true)
      setEditorError(null)

      try {
        await documentsApi.edit(documentId, markdown)
        toast.success(t('documents.editor.saveSuccess'))
        resetEditor()
        await onDocumentSaved(documentId)
      } catch (err: unknown) {
        const message = errorMessage(err, t('documents.editor.saveFailed'))
        setEditorError(message)
        toast.error(message)
      } finally {
        setEditorSaving(false)
      }
    },
    [editorAvailability, editorDocument, errorMessage, onDocumentSaved, resetEditor, t],
  )

  return {
    editorDocument,
    editorError,
    editorLoading,
    editorMarkdown,
    editorOpen,
    editorReadOnly,
    editorSaving,
    handleEditorOpenChange,
    openEditor,
    saveEditor,
  }
}

async function loadSourceEditorMarkdown(doc: DocumentItem): Promise<string> {
  const sourceHref = doc.sourceAccess?.href
  if (!sourceHref) {
    throw new Error(`source text is unavailable for ${doc.fileName}`)
  }

  const sourceText = await documentsApi.getSourceText(sourceHref)
  return serializeSourceTextForEditor(sourceText, doc.fileType)
}

function shouldLoadStoredSourceTextForEditor(doc: DocumentItem): boolean {
  return (
    doc.sourceAccess?.kind === 'stored_document' &&
    (isCodeLikeSourceFormat(doc.fileType) || isPlainTextSourceFormat(doc.fileType))
  )
}
