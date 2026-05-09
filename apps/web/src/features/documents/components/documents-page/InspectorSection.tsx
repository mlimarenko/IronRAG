import { useCallback, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { File, Loader2, Upload } from "lucide-react";
import { toast } from "sonner";

import { documentsApi, type DocumentLifecycleDetail } from "@/shared/api";
import { Button } from "@/shared/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/components/ui/dialog";
import type { DocumentItem, Locale } from "@/shared/types";

import {
  formatSize,
} from "@/features/documents/model/documentAdapter";
import { DocumentsInspectorPanel } from "@/features/documents/components/DocumentsInspectorPanel";
import { DocumentEditorShell } from "@/features/documents/components/editor/DocumentEditorShell";
import { isEditorEditableSourceFormat } from "@/features/documents/components/editor/editorSurfaceMode";
import { useDocumentEditor } from "@/features/documents/components/editor/useDocumentEditor";
import { DOCUMENT_FILE_INPUT_ACCEPT } from "@/features/documents/model/uploadAccept";

import type { UpdateSearchParamState } from "./documentsPageState";

type InspectorSectionProps = {
  activateListPollGrace: () => void;
  clearSelectedDoc: () => void;
  errorMessage: (error: unknown, fallback: string) => string;
  fetchSelectedDetail: (documentId: string) => Promise<void>;
  inspectorFacts: number | null;
  inspectorLifecycle: DocumentLifecycleDetail | null;
  inspectorSegments: number | null;
  loadFirstPage: () => Promise<void>;
  locale: Locale;
  selectedDoc: DocumentItem | null;
  selectDoc: (doc: DocumentItem) => void;
  selectionMode: boolean;
  t: TFunction;
  updateSearchParamState: UpdateSearchParamState;
};

export function InspectorSection({
  activateListPollGrace,
  clearSelectedDoc,
  errorMessage,
  fetchSelectedDetail,
  inspectorFacts,
  inspectorLifecycle,
  inspectorSegments,
  loadFirstPage,
  locale,
  selectedDoc,
  selectDoc,
  selectionMode,
  t,
  updateSearchParamState,
}: InspectorSectionProps) {
  const [deleteDocOpen, setDeleteDocOpen] = useState(false);
  const [replaceFileOpen, setReplaceFileOpen] = useState(false);
  const [replaceFile, setReplaceFile] = useState<File | null>(null);
  const [replaceLoading, setReplaceLoading] = useState(false);
  const replaceFileInputRef = useRef<HTMLInputElement>(null);

  const editAvailability = useCallback(
    (doc: DocumentItem | null) => {
      if (!doc) return { enabled: false, reason: null as string | null };
      if (!isEditorEditableSourceFormat(doc.fileType)) {
        return { enabled: false, reason: t("documents.editUnavailableFormat") };
      }
      if (
        doc.readiness === "readable" ||
        doc.readiness === "graph_sparse" ||
        doc.readiness === "graph_ready"
      ) {
        return { enabled: true, reason: null as string | null };
      }
      if (doc.readiness === "processing") {
        return { enabled: false, reason: t("documents.editUnavailableProcessing") };
      }
      if (doc.readiness === "failed") {
        return { enabled: false, reason: t("documents.editUnavailableFailed") };
      }
      return { enabled: false, reason: t("documents.editUnavailableGeneric") };
    },
    [t],
  );

  const handleDocumentEditorSaveRefresh = useCallback(
    async (documentId: string) => {
      await loadFirstPage();
      await fetchSelectedDetail(documentId);
    },
    [fetchSelectedDetail, loadFirstPage],
  );
  const documentEditor = useDocumentEditor({
    editAvailability,
    errorMessage,
    onDocumentSaved: handleDocumentEditorSaveRefresh,
    onDocumentSelected: selectDoc,
    selectedDocumentId: selectedDoc?.id ?? null,
    t,
  });

  const handleDelete = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.delete(selectedDoc.id);
      setDeleteDocOpen(false);
      clearSelectedDoc();
      await loadFirstPage();
    } catch (err) {
      toast.error(errorMessage(err, t("documents.deleteFailed")));
    }
  }, [clearSelectedDoc, errorMessage, loadFirstPage, selectedDoc, t]);

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.reprocess(selectedDoc.id);
      activateListPollGrace();
      await loadFirstPage();
      await fetchSelectedDetail(selectedDoc.id);
    } catch (err) {
      toast.error(errorMessage(err, t("documents.reprocessFailed")));
    }
  }, [
    activateListPollGrace,
    errorMessage,
    fetchSelectedDetail,
    loadFirstPage,
    selectedDoc,
    t,
  ]);

  const handleReplaceFile = useCallback(async () => {
    if (!selectedDoc || !replaceFile) return;
    setReplaceLoading(true);
    try {
      await documentsApi.replace(selectedDoc.id, replaceFile);
      toast.success(t("documents.replaceFileSuccess"));
      setReplaceFileOpen(false);
      setReplaceFile(null);
      activateListPollGrace();
      await loadFirstPage();
    } catch (err) {
      toast.error(errorMessage(err, t("documents.replaceFileFailed")));
    } finally {
      setReplaceLoading(false);
    }
  }, [
    activateListPollGrace,
    errorMessage,
    loadFirstPage,
    replaceFile,
    selectedDoc,
    t,
  ]);

  if (!selectedDoc) return null;
  const availability = editAvailability(selectedDoc);
  return (
    <>
      <DocumentsInspectorPanel
        canEdit={availability.enabled}
        editDisabledReason={availability.reason}
        inspectorFacts={inspectorFacts}
        inspectorSegments={inspectorSegments}
        lifecycle={inspectorLifecycle}
        locale={locale}
        selectedDoc={selectedDoc}
        selectionMode={selectionMode}
        setDeleteDocOpen={setDeleteDocOpen}
        setReplaceFileOpen={setReplaceFileOpen}
        t={t}
        updateSearchParamState={updateSearchParamState}
        onEdit={() => void documentEditor.openEditor(selectedDoc)}
        onRetry={handleRetry}
      />
      <Dialog open={deleteDocOpen} onOpenChange={setDeleteDocOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("documents.deleteDoc")}</DialogTitle>
            <DialogDescription>
              {t("documents.confirmDelete", { name: selectedDoc.fileName })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDocOpen(false)}>
              {t("documents.cancel")}
            </Button>
            <Button variant="destructive" onClick={() => void handleDelete()}>
              {t("documents.delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={replaceFileOpen}
        onOpenChange={(open) => {
          setReplaceFileOpen(open);
          if (!open) setReplaceFile(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("documents.replaceFileTitle")}</DialogTitle>
            <DialogDescription>
              {t("documents.replaceFileDesc", { name: selectedDoc.fileName })}
            </DialogDescription>
          </DialogHeader>
          <div
            className="border-2 border-dashed rounded-xl p-10 text-center transition-all duration-200 hover:border-primary/40 hover:bg-primary/5 cursor-pointer hover:shadow-soft"
            onClick={() => replaceFileInputRef.current?.click()}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              const file = event.dataTransfer.files[0];
              if (file) setReplaceFile(file);
            }}
          >
            <input
              ref={replaceFileInputRef}
              type="file"
              accept={DOCUMENT_FILE_INPUT_ACCEPT}
              className="hidden"
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (file) setReplaceFile(file);
                event.target.value = "";
              }}
            />
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
                <p className="text-sm font-bold">{t("documents.selectFile")}</p>
                <p className="text-xs text-muted-foreground mt-1.5">
                  {t("documents.selectFileHint")}
                </p>
              </>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setReplaceFileOpen(false);
                setReplaceFile(null);
              }}
            >
              {t("documents.cancel")}
            </Button>
            <Button disabled={!replaceFile || replaceLoading} onClick={() => void handleReplaceFile()}>
              {replaceLoading ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
                  {t("documents.replace")}...
                </>
              ) : (
                t("documents.replace")
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {documentEditor.editorDocument && (
        <DocumentEditorShell
          documentName={documentEditor.editorDocument.fileName}
          error={documentEditor.editorError}
          loading={documentEditor.editorLoading}
          markdown={documentEditor.editorMarkdown}
          onOpenChange={documentEditor.handleEditorOpenChange}
          onSave={documentEditor.saveEditor}
          open={documentEditor.editorOpen}
          saving={documentEditor.editorSaving}
          sourceFormat={documentEditor.editorDocument.fileType}
          t={t}
        />
      )}
    </>
  );
}
