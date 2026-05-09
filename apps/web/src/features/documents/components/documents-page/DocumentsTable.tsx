import type { Dispatch, SetStateAction } from "react";
import type { TFunction } from "i18next";
import { ArrowDown, ArrowUp, File, Globe, Loader2, XCircle } from "lucide-react";

import type { DocumentListSortKey, DocumentListSortOrder } from "@/shared/api";
import type { DocumentItem, Locale } from "@/shared/types";

import {
  buildDocumentStatusBadgeConfig,
  formatDate,
  formatDocumentTypeLabel,
  formatSize,
  getDocumentProcessingDurationMs,
  isWebPageDocument,
} from "@/features/documents/model/documentAdapter";

import type { LocalSortKey, LocalSortState, UploadQueueItem } from "./documentsPageState";

type DocumentsTableProps = {
  documents: DocumentItem[];
  items: DocumentItem[];
  locale: Locale;
  localSort: LocalSortState;
  onSelectDoc: (doc: DocumentItem) => void;
  onToggleLocalSort: (key: LocalSortKey) => void;
  onToggleSelection: (id: string) => void;
  onToggleSortDirection: (target: DocumentListSortKey) => void;
  pendingUploads: UploadQueueItem[];
  processingClockMs: number;
  selectedDocId: string | null;
  selectedIds: Set<string>;
  selectionMode: boolean;
  setSelectedIds: Dispatch<SetStateAction<Set<string>>>;
  sortBy: DocumentListSortKey;
  sortOrder: DocumentListSortOrder;
  t: TFunction;
};

export function DocumentsTable({
  documents,
  items,
  locale,
  localSort,
  onSelectDoc,
  onToggleLocalSort,
  onToggleSelection,
  onToggleSortDirection,
  pendingUploads,
  processingClockMs,
  selectedDocId,
  selectedIds,
  selectionMode,
  setSelectedIds,
  sortBy,
  sortOrder,
  t,
}: DocumentsTableProps) {
  const statusBadgeConfig = buildDocumentStatusBadgeConfig(t);
  const sortIcon =
    sortOrder === "asc" ? <ArrowUp className="h-3 w-3" /> : <ArrowDown className="h-3 w-3" />;
  const localSortIcon =
    localSort?.direction === "asc" ? (
      <ArrowUp className="h-3 w-3" />
    ) : (
      <ArrowDown className="h-3 w-3" />
    );
  const allVisibleSelected =
    items.length > 0 && items.every((doc) => selectedIds.has(doc.id));

  return (
    <table className="w-full min-w-[1020px] table-fixed text-sm">
      <colgroup>
        {selectionMode && <col className="w-12" />}
        <col />
        <col className="w-28" />
        <col className="w-20" />
        <col className="w-36" />
        <col className="w-24" />
        <col className="w-24" />
        <col className="w-36" />
        <col className="w-36" />
      </colgroup>
      <thead
        className="sticky top-0 z-10"
        style={{
          background:
            "linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))",
          backdropFilter: "blur(8px)",
        }}
      >
        <tr className="border-b text-left">
          {selectionMode && (
            <th className="px-4 py-3 w-10">
              <input
                type="checkbox"
                checked={allVisibleSelected}
                onChange={() =>
                  setSelectedIds((prev) => {
                    const next = new Set(prev);
                    for (const doc of items) {
                      if (allVisibleSelected) next.delete(doc.id);
                      else next.add(doc.id);
                    }
                    return next;
                  })
                }
                className="h-4 w-4 rounded border-gray-300"
              />
            </th>
          )}
          <SortHeader active={sortBy === "file_name"} icon={sortIcon} label={t("documents.name")} onClick={() => onToggleSortDirection("file_name")} />
          <SortHeader active={sortBy === "file_type"} icon={sortIcon} label={t("documents.type")} onClick={() => onToggleSortDirection("file_type")} />
          <SortHeader active={sortBy === "file_size"} icon={sortIcon} label={t("documents.size")} onClick={() => onToggleSortDirection("file_size")} />
          <SortHeader active={sortBy === "uploaded_at"} icon={sortIcon} label={t("documents.uploaded")} onClick={() => onToggleSortDirection("uploaded_at")} />
          <SortHeader active={localSort?.key === "cost"} icon={localSortIcon} label={t("documents.cost")} title={t("documents.pageLocalSortHint")} onClick={() => onToggleLocalSort("cost")} />
          <SortHeader active={localSort?.key === "time"} icon={localSortIcon} label={t("documents.pipelineTime")} title={t("documents.pageLocalSortHint")} onClick={() => onToggleLocalSort("time")} />
          <SortHeader active={localSort?.key === "finished"} icon={localSortIcon} label={t("documents.finished")} title={t("documents.pageLocalSortHint")} onClick={() => onToggleLocalSort("finished")} />
          <SortHeader active={sortBy === "status"} icon={sortIcon} label={t("documents.status")} onClick={() => onToggleSortDirection("status")} />
        </tr>
      </thead>
      <tbody>
        {pendingUploads.map((upload) => (
          <tr key={`upload-${upload.name}`} className="border-b opacity-80">
            {selectionMode && <td className="px-4 py-3.5 w-10" />}
            <td className="px-4 py-3.5">
              <DocumentNameCell fileName={upload.name} />
            </td>
            <td className="px-4 py-3.5 text-muted-foreground text-[10px]" colSpan={6} />
            <td className="px-4 py-3.5 max-w-[260px]">
              {upload.state === "error" ? (
                <span className="flex items-center gap-1.5 text-xs text-status-failed" title={upload.error ?? undefined}>
                  <XCircle className="h-3 w-3 shrink-0" />
                  <span className="truncate min-w-0">
                    {upload.error ?? t("documents.uploadFailed")}
                  </span>
                </span>
              ) : (
                <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                  <Loader2 className="h-3 w-3 animate-spin text-primary" />
                  {t("documents.uploading")}
                </span>
              )}
            </td>
          </tr>
        ))}
        {documents.map((doc) => {
          const isWebPage = isWebPageDocument(doc.sourceKind, doc.sourceUri, doc.fileName);
          const typeLabel = formatDocumentTypeLabel(doc.fileType, doc.sourceKind, t, {
            sourceUri: doc.sourceUri,
            fileName: doc.fileName,
          });
          const processingDurationMs = getDocumentProcessingDurationMs(doc, processingClockMs);
          return (
            <tr
              key={doc.id}
              className={`border-b cursor-pointer transition-all duration-150 ${
                selectedIds.has(doc.id)
                  ? "bg-primary/10"
                  : selectedDocId === doc.id
                    ? "bg-primary/5 border-l-2 border-l-primary"
                    : "hover:bg-accent/30"
              }`}
              onClick={() => (selectionMode ? onToggleSelection(doc.id) : onSelectDoc(doc))}
            >
              {selectionMode && (
                <td className="px-4 py-3.5 w-10">
                  <input
                    type="checkbox"
                    checked={selectedIds.has(doc.id)}
                    onChange={(event) => {
                      event.stopPropagation();
                      onToggleSelection(doc.id);
                    }}
                    onClick={(event) => event.stopPropagation()}
                    className="h-4 w-4 rounded border-gray-300"
                  />
                </td>
              )}
              <td className="px-4 py-3.5">
                <DocumentNameCell
                  fileName={doc.fileName}
                  isWebPage={isWebPage}
                  sourceUri={doc.sourceUri}
                />
              </td>
              <td className={`px-4 py-3.5 text-muted-foreground text-[10px] font-bold tracking-widest ${isWebPage ? "" : "uppercase"}`} title={typeLabel}>
                {typeLabel}
              </td>
              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                {formatSize(doc.fileSize)}
              </td>
              <td className="px-4 py-3.5 text-muted-foreground text-xs">
                {formatDate(doc.uploadedAt, locale)}
              </td>
              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                {doc.cost != null ? `$${doc.cost.toFixed(3)}` : "\u2014"}
              </td>
              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                {processingDurationMs != null ? `${Math.floor(processingDurationMs / 1000)}s` : "\u2014"}
              </td>
              <td className="px-4 py-3.5 text-muted-foreground text-xs">
                {doc.processingFinishedAt ? formatDate(doc.processingFinishedAt, locale) : "\u2014"}
              </td>
              <td className="px-4 py-3.5">
                <span className={`status-badge ${statusBadgeConfig[doc.status].cls}`} title={doc.statusReason}>
                  {statusBadgeConfig[doc.status].label}
                </span>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function SortHeader({
  active,
  icon,
  label,
  onClick,
  title,
}: {
  active: boolean;
  icon: JSX.Element;
  label: string;
  onClick: () => void;
  title?: string;
}) {
  return (
    <th className="px-4 py-3 section-label">
      <button
        className="flex items-center gap-1 hover:text-foreground transition-colors"
        title={title}
        onClick={onClick}
      >
        {label}
        {active && icon}
      </button>
    </th>
  );
}

function DocumentNameCell({
  fileName,
  isWebPage = false,
  sourceUri,
}: {
  fileName: string;
  isWebPage?: boolean;
  sourceUri?: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div
        className={`w-8 h-8 rounded-xl flex items-center justify-center shrink-0 ${
          isWebPage ? "bg-blue-100 dark:bg-blue-900/30" : "bg-surface-sunken"
        }`}
      >
        {isWebPage ? (
          <Globe className="h-3.5 w-3.5 text-blue-600 dark:text-blue-400" />
        ) : (
          <File className="h-3.5 w-3.5 text-muted-foreground" />
        )}
      </div>
      <div className="min-w-0 flex-1">
        <span className="block truncate font-semibold" title={fileName}>
          {fileName}
        </span>
        {isWebPage && sourceUri && sourceUri !== fileName && (
          <span className="block truncate text-[10px] text-muted-foreground" title={sourceUri}>
            {sourceUri}
          </span>
        )}
      </div>
    </div>
  );
}
