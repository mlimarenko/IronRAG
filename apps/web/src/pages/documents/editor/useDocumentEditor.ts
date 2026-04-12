import { useCallback, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';

import { documentsApi } from '@/api';
import type { DocumentItem } from '@/types';

import { buildEditorBlocks, serializeEditorBlocks, serializeSourceTextForEditor } from './documentEditorBlocks';
import { isCodeLikeSourceFormat } from './editorSurfaceMode';

type EditAvailability = {
  enabled: boolean;
  reason: string | null;
};

type UseDocumentEditorOptions = {
  editAvailability: (doc: DocumentItem | null) => EditAvailability;
  errorMessage: (error: unknown, fallback: string) => string;
  onDocumentSaved: (documentId: string) => Promise<void>;
  onDocumentSelected: (doc: DocumentItem) => void | Promise<void>;
  selectedDocumentId: string | null;
  t: TFunction;
};

export function useDocumentEditor({
  editAvailability,
  errorMessage,
  onDocumentSaved,
  onDocumentSelected,
  selectedDocumentId,
  t,
}: UseDocumentEditorOptions) {
  const [editorOpen, setEditorOpen] = useState(false);
  const [editorLoading, setEditorLoading] = useState(false);
  const [editorSaving, setEditorSaving] = useState(false);
  const [editorMarkdown, setEditorMarkdown] = useState('');
  const [editorError, setEditorError] = useState<string | null>(null);
  const [editorDocument, setEditorDocument] = useState<DocumentItem | null>(null);

  const resetEditor = useCallback(() => {
    setEditorOpen(false);
    setEditorDocument(null);
    setEditorMarkdown('');
    setEditorError(null);
    setEditorLoading(false);
    setEditorSaving(false);
  }, []);

  const handleEditorOpenChange = useCallback(
    (open: boolean) => {
      if (open) {
        setEditorOpen(true);
        return;
      }
      resetEditor();
    },
    [resetEditor],
  );

  const openEditor = useCallback(
    async (doc: DocumentItem) => {
      const availability = editAvailability(doc);
      if (!availability.enabled) {
        toast.error(availability.reason ?? t('documents.editUnavailableGeneric'));
        return;
      }

      if (selectedDocumentId !== doc.id) {
        void onDocumentSelected(doc);
      }

      setEditorDocument(doc);
      setEditorMarkdown('');
      setEditorError(null);
      setEditorLoading(true);
      setEditorOpen(true);

      try {
        const nextMarkdown = isCodeLikeSourceFormat(doc.fileType)
          ? await loadCodeEditorMarkdown(doc)
          : await loadStructuredEditorMarkdown(doc);
        setEditorMarkdown(nextMarkdown);
      } catch (err: unknown) {
        setEditorError(errorMessage(err, t('documents.editor.loadFailed')));
      } finally {
        setEditorLoading(false);
      }
    },
    [
      editAvailability,
      errorMessage,
      onDocumentSelected,
      selectedDocumentId,
      t,
    ],
  );

  const saveEditor = useCallback(
    async (markdown: string) => {
      if (!editorDocument) {
        return;
      }

      const documentId = editorDocument.id;
      setEditorSaving(true);
      setEditorError(null);

      try {
        await documentsApi.edit(documentId, markdown);
        toast.success(t('documents.editor.saveSuccess'));
        resetEditor();
        await onDocumentSaved(documentId);
      } catch (err: unknown) {
        const message = errorMessage(err, t('documents.editor.saveFailed'));
        setEditorError(message);
        toast.error(message);
      } finally {
        setEditorSaving(false);
      }
    },
    [editorDocument, errorMessage, onDocumentSaved, resetEditor, t],
  );

  return {
    editorDocument,
    editorError,
    editorLoading,
    editorMarkdown,
    editorOpen,
    editorSaving,
    handleEditorOpenChange,
    openEditor,
    saveEditor,
  };
}

async function loadStructuredEditorMarkdown(doc: DocumentItem): Promise<string> {
  const segments = await documentsApi.getPreparedSegments(doc.id);
  return serializeEditorBlocks(buildEditorBlocks(segments, doc.fileType));
}

async function loadCodeEditorMarkdown(doc: DocumentItem): Promise<string> {
  const sourceHref = doc.sourceAccess?.href;
  if (!sourceHref) {
    throw new Error(`source text is unavailable for ${doc.fileName}`);
  }

  const sourceText = await documentsApi.getSourceText(sourceHref);
  return serializeSourceTextForEditor(sourceText, doc.fileType);
}
