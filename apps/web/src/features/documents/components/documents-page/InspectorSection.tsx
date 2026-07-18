import { Suspense, lazy, useCallback, useRef, useState } from 'react'
import type { TFunction } from 'i18next'
import { File, Loader2, Upload } from 'lucide-react'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'

import { documentsApi, type DocumentLifecycleDetail } from '@/shared/api'
import { Button } from '@/shared/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog'
import type { DocumentItem, Locale } from '@/shared/types'

import { formatSize } from '@/features/documents/model/documentAdapter'
import { DocumentsInspectorPanel } from '@/features/documents/components/DocumentsInspectorPanel'
// Tiptap (StarterKit + table/image/markdown extensions ≈ 485 KB) lives entirely
// inside DocumentEditorShell. It is only needed once the operator opens the
// editor overlay, so it is split out of the synchronous DocumentsPage chunk and
// fetched on demand. Browsing the document list no longer pays the editor cost.
const DocumentEditorShell = lazy(() =>
  import('@/features/documents/components/editor/DocumentEditorShell').then((module) => ({
    default: module.DocumentEditorShell,
  })),
)
import { isEditorEditableSourceFormat } from '@/features/documents/components/editor/editorSurfaceMode'
import { useDocumentEditor } from '@/features/documents/components/editor/useDocumentEditor'
import { DOCUMENT_FILE_INPUT_ACCEPT } from '@/features/documents/model/uploadAccept'

import type { UpdateSearchParamState } from './documentsPageState'

type InspectorSectionProps = {
  activateListPollGrace: () => void
  canEdit: boolean
  canDelete: boolean
  clearSelectedDoc: () => void
  documentHintEditable: boolean
  errorMessage: (error: unknown, fallback: string) => string
  fetchSelectedDetail: (documentId: string) => Promise<void>
  inspectorLifecycle: DocumentLifecycleDetail | null
  loadFirstPage: () => Promise<void>
  locale: Locale
  selectedDoc: DocumentItem | null
  selectDoc: (doc: DocumentItem) => void
  selectionMode: boolean
  t: TFunction
  updateDocumentHintLocally: (documentId: string, documentHint: string | null) => void
  updateSearchParamState: UpdateSearchParamState
}

export function InspectorSection({
  activateListPollGrace,
  canEdit,
  canDelete,
  clearSelectedDoc,
  documentHintEditable,
  errorMessage,
  fetchSelectedDetail,
  inspectorLifecycle,
  loadFirstPage,
  locale,
  selectedDoc,
  selectDoc,
  selectionMode,
  t,
  updateDocumentHintLocally,
  updateSearchParamState,
}: Readonly<InspectorSectionProps>) {
  const navigate = useNavigate()
  const [deleteDocOpen, setDeleteDocOpen] = useState(false)
  const [replaceFileOpen, setReplaceFileOpen] = useState(false)
  const [replaceFile, setReplaceFile] = useState<File | null>(null)
  const [replaceLoading, setReplaceLoading] = useState(false)
  const replaceFileInputRef = useRef<HTMLInputElement>(null)

  const editorAvailability = useCallback(
    (doc: DocumentItem | null) => {
      if (!doc) return { enabled: false, readOnly: false, reason: null as string | null }
      if (
        doc.readiness === 'readable' ||
        doc.readiness === 'graph_sparse' ||
        doc.readiness === 'graph_ready'
      ) {
        return {
          enabled: true,
          readOnly: !isEditorEditableSourceFormat(doc.fileType),
          reason: null as string | null,
        }
      }
      if (doc.readiness === 'processing') {
        return {
          enabled: false,
          readOnly: false,
          reason: t('documents.editUnavailableProcessing'),
        }
      }
      if (doc.readiness === 'failed') {
        return {
          enabled: false,
          readOnly: false,
          reason: t('documents.editUnavailableFailed'),
        }
      }
      return {
        enabled: false,
        readOnly: false,
        reason: t('documents.editUnavailableGeneric'),
      }
    },
    [t],
  )

  const handleDocumentEditorSaveRefresh = useCallback(
    async (documentId: string) => {
      await loadFirstPage()
      await fetchSelectedDetail(documentId)
    },
    [fetchSelectedDetail, loadFirstPage],
  )
  const documentEditor = useDocumentEditor({
    editorAvailability,
    errorMessage,
    onDocumentSaved: handleDocumentEditorSaveRefresh,
    onDocumentSelected: selectDoc,
    selectedDocumentId: selectedDoc?.id ?? null,
    t,
  })

  const handleDelete = useCallback(async () => {
    if (!selectedDoc) return
    try {
      await documentsApi.delete(selectedDoc.id)
      setDeleteDocOpen(false)
      clearSelectedDoc()
      await loadFirstPage()
    } catch (err) {
      toast.error(errorMessage(err, t('documents.deleteFailed')))
    }
  }, [clearSelectedDoc, errorMessage, loadFirstPage, selectedDoc, t])

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return
    try {
      await documentsApi.reprocess(selectedDoc.id)
      activateListPollGrace()
      await loadFirstPage()
      await fetchSelectedDetail(selectedDoc.id)
    } catch (err) {
      toast.error(errorMessage(err, t('documents.reprocessFailed')))
    }
  }, [activateListPollGrace, errorMessage, fetchSelectedDetail, loadFirstPage, selectedDoc, t])

  const handleReplaceFile = useCallback(async () => {
    if (!selectedDoc || !replaceFile) return
    setReplaceLoading(true)
    try {
      await documentsApi.replace(selectedDoc.id, replaceFile)
      toast.success(t('documents.replaceFileSuccess'))
      setReplaceFileOpen(false)
      setReplaceFile(null)
      activateListPollGrace()
      await loadFirstPage()
    } catch (err) {
      toast.error(errorMessage(err, t('documents.replaceFileFailed')))
    } finally {
      setReplaceLoading(false)
    }
  }, [activateListPollGrace, errorMessage, loadFirstPage, replaceFile, selectedDoc, t])

  if (!selectedDoc) return null
  const availability = editorAvailability(selectedDoc)
  const isGraphReady =
    selectedDoc.readiness === 'readable' ||
    selectedDoc.readiness === 'graph_sparse' ||
    selectedDoc.readiness === 'graph_ready'
  const inspectorPanel = (
    <DocumentsInspectorPanel
      canEdit={canEdit}
      canDelete={canDelete}
      documentHintEditable={documentHintEditable}
      editorActionDisabledReason={availability.reason}
      editorActionEnabled={availability.enabled}
      editorActionReadOnly={availability.readOnly}
      formatErrorMessage={errorMessage}
      lifecycle={inspectorLifecycle}
      locale={locale}
      selectedDoc={selectedDoc}
      selectionMode={selectionMode}
      setDeleteDocOpen={setDeleteDocOpen}
      setReplaceFileOpen={setReplaceFileOpen}
      t={t}
      updateSearchParamState={updateSearchParamState}
      onDocumentHintUpdated={updateDocumentHintLocally}
      onOpenEditor={async () => {
        await documentEditor.openEditor(selectedDoc)
      }}
      onRetry={handleRetry}
      onViewInGraph={
        isGraphReady
          ? () => navigate(`/graph?nodeId=${encodeURIComponent(selectedDoc.id)}`)
          : undefined
      }
    />
  )

  return (
    <>
      {inspectorPanel}
      <Dialog open={deleteDocOpen} onOpenChange={setDeleteDocOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('documents.deleteDoc')}</DialogTitle>
            <DialogDescription>
              {t('documents.confirmDelete', { name: selectedDoc.fileName })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDocOpen(false)}>
              {t('documents.cancel')}
            </Button>
            <Button variant="destructive" onClick={handleDelete}>
              {t('documents.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={replaceFileOpen}
        onOpenChange={(open) => {
          setReplaceFileOpen(open)
          if (!open) setReplaceFile(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('documents.replaceFileTitle')}</DialogTitle>
            <DialogDescription>
              {t('documents.replaceFileDesc', { name: selectedDoc.fileName })}
            </DialogDescription>
          </DialogHeader>
          <div
            className="block rounded-lg border border-dashed border-border bg-surface-sunken/40 text-center transition-all duration-200 hover:border-primary/40 hover:bg-primary/5 hover:shadow-soft focus-within:border-primary/60 focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2"
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault()
              const file = event.dataTransfer.files[0]
              if (file) setReplaceFile(file)
            }}
          >
            <input
              ref={replaceFileInputRef}
              id="replace-document-file"
              type="file"
              accept={DOCUMENT_FILE_INPUT_ACCEPT}
              className="sr-only"
              onChange={(event) => {
                const file = event.target.files?.[0]
                if (file) setReplaceFile(file)
                event.target.value = ''
              }}
            />
            <label htmlFor="replace-document-file" className="block cursor-pointer p-10">
              {replaceFile ? (
                <>
                  <File className="h-8 w-8 text-primary mx-auto mb-3" />
                  <p className="text-sm font-bold">{replaceFile.name}</p>
                  <p className="text-xs text-muted-foreground mt-1.5">
                    {formatSize(replaceFile.size)}
                  </p>
                </>
              ) : (
                <>
                  <Upload className="h-8 w-8 text-muted-foreground mx-auto mb-3" />
                  <p className="text-sm font-bold">{t('documents.selectFile')}</p>
                  <p className="text-xs text-muted-foreground mt-1.5">
                    {t('documents.selectFileHint')}
                  </p>
                </>
              )}
            </label>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setReplaceFileOpen(false)
                setReplaceFile(null)
              }}
            >
              {t('documents.cancel')}
            </Button>
            <Button disabled={!replaceFile || replaceLoading} onClick={handleReplaceFile}>
              {replaceLoading ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
                  {t('documents.replace')}...
                </>
              ) : (
                t('documents.replace')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {documentEditor.editorDocument && (
        <Suspense fallback={null}>
          <DocumentEditorShell
            documentName={documentEditor.editorDocument.fileName}
            error={documentEditor.editorError}
            loading={documentEditor.editorLoading}
            markdown={documentEditor.editorMarkdown}
            onOpenChange={documentEditor.handleEditorOpenChange}
            onSave={documentEditor.saveEditor}
            open={documentEditor.editorOpen}
            readOnly={documentEditor.editorReadOnly}
            saving={documentEditor.editorSaving}
            sourceFormat={documentEditor.editorDocument.fileType}
            sourceHref={documentEditor.editorDocument.sourceAccess?.href}
            t={t}
          />
        </Suspense>
      )}
    </>
  )
}
