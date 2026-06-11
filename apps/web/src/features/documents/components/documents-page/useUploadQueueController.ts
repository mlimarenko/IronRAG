import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type DragEvent,
} from "react";
import type { TFunction } from "i18next";
import { toast } from "sonner";

import { documentsApi } from "@/shared/api";
import { buildUploadFailureNotice } from "@/shared/lib/document-processing";
import type { DocumentItem, Library } from "@/shared/types";

import {
  buildUploadCandidates,
  normalizeUploadName,
  type UploadCandidate,
} from "@/features/documents/model/uploadCandidates";

import type { UploadQueueItem } from "./documentsPageState";

type UploadQueueControllerInput = {
  activeLibrary: Library | null;
  activateListPollGrace: () => void;
  errorMessage: (error: unknown, fallback: string) => string;
  items: DocumentItem[];
  loadFirstPage: () => Promise<void>;
  t: TFunction;
};

type PendingUploadSelection = {
  candidates: UploadCandidate[];
};

export function useUploadQueueController({
  activeLibrary,
  activateListPollGrace,
  errorMessage,
  items,
  loadFirstPage,
  t,
}: UploadQueueControllerInput) {
  const [dragOver, setDragOver] = useState(false);
  const [uploadDialogHint, setUploadDialogHint] = useState("");
  const [pendingUploadSelection, setPendingUploadSelection] =
    useState<PendingUploadSelection | null>(null);
  const [uploadQueue, setUploadQueue] = useState<UploadQueueItem[]>([]);
  const [duplicateConflict, setDuplicateConflict] = useState<{
    candidate: UploadCandidate;
    documentHint: string;
    existingDocId: string;
    remaining: UploadCandidate[];
  } | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);

  const doUploadFile = useCallback(
    async (candidate: UploadCandidate, documentHint: string) => {
      if (!activeLibrary) return;
      setUploadQueue((prev) => [...prev, { name: candidate.name, state: "uploading" }]);
      try {
        const trimmedDocumentHint = documentHint.trim();
        await documentsApi.upload(activeLibrary.id, candidate.file, {
          documentHint: trimmedDocumentHint || undefined,
          externalKey: candidate.name,
          fileName: candidate.file.name,
          title: candidate.name,
        });
        activateListPollGrace();
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === candidate.name ? { ...item, state: "done" } : item,
          ),
        );
      } catch (err) {
        const notice = buildUploadFailureNotice(err, errorMessage(err, t("documents.uploadFailed")), t);
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === candidate.name
              ? {
                  ...item,
                  state: "error",
                  error: notice.summary,
                  errorAction: notice.action,
                  errorDiagnosticCode: notice.diagnosticCode,
                  errorDiagnosticMessage: notice.diagnosticMessage,
                }
              : item,
          ),
        );
      }
    },
    [activeLibrary, activateListPollGrace, errorMessage, t],
  );

  const doReplaceFile = useCallback(
    async (docId: string, file: File, uploadName = file.name) => {
      setUploadQueue((prev) => [...prev, { name: uploadName, state: "uploading" }]);
      try {
        await documentsApi.replace(docId, file);
        activateListPollGrace();
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === uploadName ? { ...item, state: "done" } : item,
          ),
        );
      } catch (err) {
        const notice = buildUploadFailureNotice(
          err,
          errorMessage(err, t("documents.replaceFileFailed")),
          t,
        );
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === uploadName
              ? {
                  ...item,
                  state: "error",
                  error: notice.summary,
                  errorAction: notice.action,
                  errorDiagnosticCode: notice.diagnosticCode,
                  errorDiagnosticMessage: notice.diagnosticMessage,
                }
              : item,
          ),
        );
      }
    },
    [activateListPollGrace, errorMessage, t],
  );

  const finalizeUpload = useCallback(async () => {
    await loadFirstPage();
    setUploadQueue((prev) => {
      const failed = prev.filter((item) => item.state === "error");
      if (failed.length > 0) {
        const firstFailure = failed[0];
        toast.error(
          firstFailure?.errorAction
            ? `${t("documents.uploadBatchFailed", { count: failed.length })}: ${firstFailure.errorAction}`
            : t("documents.uploadBatchFailed", { count: failed.length }),
        );
      }
      return failed;
    });
  }, [loadFirstPage, t]);

  const processUploadQueue = useCallback(
    async (candidates: UploadCandidate[], documentHint: string) => {
      let remaining = candidates;
      while (activeLibrary && remaining.length > 0) {
        const candidate = remaining[0];
        if (!candidate) break;
        const rest = remaining.slice(1);
        const existing = items.find(
          (doc) =>
            normalizeUploadName(doc.fileName).toLowerCase() ===
            candidate.name.toLowerCase(),
        );
        if (existing) {
          setDuplicateConflict({
            candidate,
            documentHint,
            existingDocId: existing.id,
            remaining: rest,
          });
          return;
        }
        await doUploadFile(candidate, documentHint);
        remaining = rest;
      }
      await finalizeUpload();
    },
    [activeLibrary, doUploadFile, finalizeUpload, items],
  );

  const uploadFiles = useCallback(
    (files: File[]) => {
      if (!activeLibrary) return;
      const candidates = buildUploadCandidates(files);
      if (candidates.length === 0) return;
      setPendingUploadSelection((current) => current ?? { candidates });
    },
    [activeLibrary],
  );
  const handleFileSelect = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      uploadFiles(Array.from(event.target.files ?? []));
      event.target.value = "";
    },
    [uploadFiles],
  );
  const handleDrop = useCallback(
    (event: DragEvent) => {
      event.preventDefault();
      setDragOver(false);
      uploadFiles(Array.from(event.dataTransfer.files));
    },
    [uploadFiles],
  );
  const cancelUploadDialog = useCallback(() => {
    setPendingUploadSelection(null);
    setUploadDialogHint("");
  }, []);
  const confirmUploadDialog = useCallback(async () => {
    if (!pendingUploadSelection) return;
    const { candidates } = pendingUploadSelection;
    const documentHint = uploadDialogHint.trim();
    setPendingUploadSelection(null);
    setUploadDialogHint("");
    await processUploadQueue(candidates, documentHint);
  }, [pendingUploadSelection, processUploadQueue, uploadDialogHint]);
  const resolveDuplicate = useCallback(
    async (mode: "replace" | "add" | "skip") => {
      if (!duplicateConflict) return;
      const { candidate, documentHint, existingDocId, remaining } =
        duplicateConflict;
      setDuplicateConflict(null);
      if (mode === "replace") {
        await doReplaceFile(existingDocId, candidate.file, candidate.name);
      } else if (mode === "add") {
        await doUploadFile(candidate, documentHint);
      }
      await processUploadQueue(remaining, documentHint);
    },
    [doReplaceFile, doUploadFile, duplicateConflict, processUploadQueue],
  );

  return {
    cancelUploadDialog,
    confirmUploadDialog,
    dragOver,
    duplicateConflict,
    fileInputRef,
    folderInputRef,
    handleFileSelect,
    handleFolderSelect: handleFileSelect,
    pendingUploads: useMemo(
      () => uploadQueue.filter((item) => item.state !== "done"),
      [uploadQueue],
    ),
    resolveDuplicate,
    setUploadDialogHint,
    uploadDialogFileCount: pendingUploadSelection?.candidates.length ?? 0,
    uploadDialogHint,
    uploadDialogOpen: Boolean(pendingUploadSelection),
    dropTargetProps: {
      onDragLeave: () => setDragOver(false),
      onDragOver: (event: DragEvent) => {
        event.preventDefault();
        setDragOver(true);
      },
      onDrop: handleDrop,
    },
  };
}

export type UploadQueueController = ReturnType<typeof useUploadQueueController>;
