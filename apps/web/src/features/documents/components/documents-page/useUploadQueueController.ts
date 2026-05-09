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

export function useUploadQueueController({
  activeLibrary,
  activateListPollGrace,
  errorMessage,
  items,
  loadFirstPage,
  t,
}: UploadQueueControllerInput) {
  const [dragOver, setDragOver] = useState(false);
  const [uploadQueue, setUploadQueue] = useState<UploadQueueItem[]>([]);
  const [duplicateConflict, setDuplicateConflict] = useState<{
    candidate: UploadCandidate;
    existingDocId: string;
    remaining: UploadCandidate[];
  } | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);

  const doUploadFile = useCallback(
    async (candidate: UploadCandidate) => {
      if (!activeLibrary) return;
      setUploadQueue((prev) => [...prev, { name: candidate.name, state: "uploading" }]);
      try {
        await documentsApi.upload(activeLibrary.id, candidate.file, {
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
        const message = errorMessage(err, t("documents.uploadFailed"));
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === candidate.name
              ? { ...item, state: "error", error: message }
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
        const message = errorMessage(err, t("documents.replaceFileFailed"));
        setUploadQueue((prev) =>
          prev.map((item) =>
            item.name === uploadName
              ? { ...item, state: "error", error: message }
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
        toast.error(t("documents.uploadBatchFailed", { count: failed.length }));
      }
      return failed;
    });
  }, [loadFirstPage, t]);

  const processUploadQueue = useCallback(
    async (candidates: UploadCandidate[]) => {
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
          setDuplicateConflict({ candidate, existingDocId: existing.id, remaining: rest });
          return;
        }
        await doUploadFile(candidate);
        remaining = rest;
      }
      await finalizeUpload();
    },
    [activeLibrary, doUploadFile, finalizeUpload, items],
  );

  const uploadFiles = useCallback(
    async (files: File[]) => {
      if (!activeLibrary) return;
      await processUploadQueue(buildUploadCandidates(files));
    },
    [activeLibrary, processUploadQueue],
  );
  const handleFileSelect = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      void uploadFiles(Array.from(event.target.files ?? []));
      event.target.value = "";
    },
    [uploadFiles],
  );
  const handleDrop = useCallback(
    (event: DragEvent) => {
      event.preventDefault();
      setDragOver(false);
      void uploadFiles(Array.from(event.dataTransfer.files));
    },
    [uploadFiles],
  );
  const resolveDuplicate = useCallback(
    async (mode: "replace" | "add" | "skip") => {
      if (!duplicateConflict) return;
      const { candidate, existingDocId, remaining } = duplicateConflict;
      setDuplicateConflict(null);
      if (mode === "replace") {
        await doReplaceFile(existingDocId, candidate.file, candidate.name);
      } else if (mode === "add") {
        await doUploadFile(candidate);
      }
      await processUploadQueue(remaining);
    },
    [doReplaceFile, doUploadFile, duplicateConflict, processUploadQueue],
  );

  return {
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
