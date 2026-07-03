import type { TFunction } from "i18next";
import type { Dispatch, SetStateAction } from "react";
import { FileText, Loader2, MousePointerSquareDashed, RotateCw, Upload, XCircle } from "lucide-react";

import type {
  DocumentListPageResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentListStatusFilter,
} from "@/shared/api";
import { Button } from "@/shared/components/ui/button";
import { DataState } from "@/shared/components/DataState";
import { WorkbenchEmptyState } from "@/shared/components/layout/WorkbenchEmptyState";
import type { DocumentItem, Library, Locale } from "@/shared/types";

import { BulkRerunProgressBanner } from "@/features/documents/components/BulkRerunProgressBanner";

import { BulkSelectionBar } from "./BulkSelectionBar";
import { DocumentsFiltersBar } from "./DocumentsFiltersBar";
import { DocumentsPaginationFooter } from "./DocumentsPaginationFooter";
import { DocumentsTable } from "./DocumentsTable";
import type {
  DocumentsTableState,
  DocumentsStatusBucket,
  LocalSortState,
  PageSizeOption,
  SortValue,
  UpdateSearchParamState,
  UploadQueueItem,
} from "./documentsPageState";
import type { UploadQueueController } from "./useUploadQueueController";
import { useDocumentsListController } from "./useDocumentsListController";

type PaginationState = {
  canGoNext: boolean;
  canGoPrevious: boolean;
  currentPageNumber: number;
  goToNextPage: () => void;
  goToPreviousPage: () => void;
  goToPage: (target: number) => void;
  pageSize: PageSizeOption;
  show: boolean;
  totalPages: number | null;
  visibleRangeEnd: number;
  visibleRangeStart: number;
};

type DocumentsListSectionProps = {
  activeLibrary: Library;
  activateListPollGrace: () => void;
  canUpload: boolean;
  debouncedSearch: string;
  errorMessage: (error: unknown, fallback: string) => string;
  filteredTotal: number | null;
  isLoading: boolean;
  items: DocumentItem[];
  libraryCost: number;
  loadError: string | null;
  loadFirstPage: () => Promise<void>;
  locale: Locale;
  localSort: LocalSortState;
  onSelectionModeChange: (selectionMode: boolean) => void;
  pageSize: PageSizeOption;
  pagination: PaginationState;
  pendingUploads: UploadQueueItem[];
  searchQuery: string;
  selectedDoc: DocumentItem | null;
  selectDoc: (doc: DocumentItem) => void;
  showDetailColumns: boolean;
  sortBy: DocumentListSortKey;
  sortOrder: DocumentListSortOrder;
  sortValue: SortValue;
  statusBackendFilter: DocumentListStatusFilter[];
  statusBucket: DocumentsStatusBucket;
  statusCounts: DocumentListPageResponse["statusCounts"] | null;
  t: TFunction;
  setTableState: Dispatch<SetStateAction<DocumentsTableState>>;
  updateSearchParamState: UpdateSearchParamState;
  uploadController: UploadQueueController;
  workspaceCost: number;
};

export function DocumentsListSection(props: DocumentsListSectionProps) {
  const list = useDocumentsListController({
    activeLibrary: props.activeLibrary,
    activateListPollGrace: props.activateListPollGrace,
    debouncedSearch: props.debouncedSearch,
    errorMessage: props.errorMessage,
    filteredTotal: props.filteredTotal,
    items: props.items,
    loadFirstPage: props.loadFirstPage,
    localSort: props.localSort,
    onSelectionModeChange: props.onSelectionModeChange,
    pageSize: props.pageSize,
    sortBy: props.sortBy,
    sortOrder: props.sortOrder,
    sortValue: props.sortValue,
    statusBackendFilter: props.statusBackendFilter,
    t: props.t,
    setTableState: props.setTableState,
    updateSearchParamState: props.updateSearchParamState,
  });

  // The drop target is only live for roles that can upload; viewers get the
  // ordinary list with no ghost drag affordance (DOC-01 / role matrix §8).
  const dropTargetProps = props.canUpload ? props.uploadController.dropTargetProps : {};
  const dragActive = props.canUpload && props.uploadController.dragOver;

  return (
    <>
      <DocumentsFiltersBar
        libraryCost={props.libraryCost}
        onCancelSelection={list.clearSelection}
        onStartSelection={() => list.setSelectionMode(true)}
        onToggleDetailColumns={list.toggleDetailColumns}
        searchQuery={props.searchQuery}
        selectionMode={list.selectionMode}
        showDetailColumns={props.showDetailColumns}
        statusBucket={props.statusBucket}
        statusCounts={props.statusCounts ?? null}
        t={props.t}
        updateSearchParamState={props.updateSearchParamState}
        workspaceCost={props.workspaceCost}
      />
      <div
        className={`relative flex-1 min-w-0 overflow-hidden ${dragActive ? "ring-2 ring-primary ring-inset bg-primary/5" : ""}`}
        {...dropTargetProps}
      >
        {list.bulkProgress && (
          <div className="mx-4 mt-4">
            <BulkRerunProgressBanner
              bulkRerun={list.bulkProgress}
              onDismiss={() => list.setBulkRerun(null)}
              t={props.t}
            />
          </div>
        )}
        {list.showSelectAllMatching && (
          <SelectAllMatchingBanner
            expanding={list.expandingSelection}
            onSelectAll={() => void list.selectAllMatching()}
            selectedCount={list.selectedIds.size}
            t={props.t}
            total={props.filteredTotal ?? props.items.length}
          />
        )}
        {dragActive && <DropOverlay t={props.t} />}
        <DataState
          query={{
            isLoading: props.isLoading && props.items.length === 0,
            error:
              props.loadError && props.items.length === 0
                ? props.loadError
                : null,
            data: props.items,
          }}
          loading={<LoadingState t={props.t} />}
          errorRender={(error) => (
            <ErrorState
              error={String(error)}
              onRetry={() => void props.loadFirstPage()}
              t={props.t}
            />
          )}
          emptyCheck={() =>
            props.items.length === 0 && props.pendingUploads.length === 0
          }
          emptyRender={
            <EmptyState
              canUpload={props.canUpload}
              searchQuery={props.searchQuery}
              t={props.t}
            />
          }
        >
          {() => (
            <div className="flex h-full min-h-0 flex-col">
              <div className="min-h-0 flex-1 overflow-auto">
                <DocumentsTable
                  documents={list.displayedItems}
                  items={props.items}
                  locale={props.locale}
                  localSort={list.localSort}
                  onSelectDoc={props.selectDoc}
                  onToggleLocalSort={list.toggleLocalSort}
                  onToggleSelection={list.toggleSelection}
                  onToggleSortDirection={list.toggleSortDirection}
                  pendingUploads={props.pendingUploads}
                  processingClockMs={list.processingClockMs}
                  selectedDocId={props.selectedDoc?.id ?? null}
                  selectedIds={list.selectedIds}
                  selectionMode={list.selectionMode}
                  setSelectedIds={list.setSelectedIds}
                  showDetailColumns={props.showDetailColumns}
                  sortBy={props.sortBy}
                  sortOrder={props.sortOrder}
                  t={props.t}
                />
                {props.canUpload && !list.selectionMode && (
                  <DragHintFooter t={props.t} />
                )}
              </div>
              {props.pagination.show && (
                <DocumentsPaginationFooter
                  {...props.pagination}
                  filteredTotal={props.filteredTotal}
                  itemCount={props.items.length}
                  t={props.t}
                  updateSearchParamState={props.updateSearchParamState}
                />
              )}
            </div>
          )}
        </DataState>
      </div>
      <BulkSelectionBar
        clearSelection={list.clearSelection}
        onBulkCancel={() => void list.handleBulkCancel()}
        onBulkDelete={() => void list.handleBulkDelete()}
        onBulkReprocess={() => void list.handleBulkReprocess()}
        selectedCount={list.selectedIds.size}
        t={props.t}
      />
    </>
  );
}

function SelectAllMatchingBanner({
  expanding,
  onSelectAll,
  selectedCount,
  t,
  total,
}: {
  expanding: boolean;
  onSelectAll: () => void;
  selectedCount: number;
  t: TFunction;
  total: number;
}) {
  return (
    <div className="mx-4 mt-4 rounded-xl border border-primary/20 bg-primary/5 px-4 py-2.5 text-sm flex items-center justify-between gap-3">
      <span>{t("documents.selectAllBannerSelected", { count: selectedCount })}</span>
      <Button size="sm" variant="default" className="h-7 text-xs shrink-0" disabled={expanding} onClick={onSelectAll}>
        {expanding ? (
          <>
            <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
            {t("documents.selectAllBannerExpanding")}
          </>
        ) : (
          t("documents.selectAllBannerAction", { total })
        )}
      </Button>
    </div>
  );
}

function DropOverlay({ t }: { t: TFunction }) {
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center pointer-events-none">
      <div className="rounded-xl border border-dashed border-primary/70 bg-card/95 px-5 py-4 shadow-lifted">
        <Upload className="mx-auto mb-2 h-5 w-5 text-primary" />
        <p className="text-sm font-semibold">{t("documents.dropToUpload")}</p>
      </div>
    </div>
  );
}

/**
 * Persistent, low-key hint that the list is also a drop target (DOC-02). It
 * sits under the table so the affordance is discoverable before the user ever
 * begins a drag, which is exactly when the DropOverlay is invisible.
 */
function DragHintFooter({ t }: { t: TFunction }) {
  return (
    <div className="mx-4 my-3 flex items-center justify-center gap-2 rounded-lg border border-dashed border-border bg-surface-sunken/40 px-4 py-2.5 text-xs text-muted-foreground">
      <MousePointerSquareDashed className="h-3.5 w-3.5 shrink-0 text-primary/70" />
      <span>{t("documents.dragHint")}</span>
    </div>
  );
}

function LoadingState({ t }: { t: TFunction }) {
  return (
    <WorkbenchEmptyState
      icon={<Loader2 className="h-7 w-7 animate-spin text-primary/70" />}
      title={t("documents.loadingDocs")}
    />
  );
}

function ErrorState({
  error,
  onRetry,
  t,
}: {
  error: string;
  onRetry: () => void;
  t: TFunction;
}) {
  return (
    <WorkbenchEmptyState
      icon={<XCircle className="h-7 w-7 text-destructive" />}
      title={t("documents.failedToLoad")}
      description={error}
      action={
        <Button size="sm" variant="outline" onClick={onRetry}>
          <RotateCw className="h-3.5 w-3.5 mr-1.5" />
          {t("documents.retry")}
        </Button>
      }
    />
  );
}

function EmptyState({
  canUpload,
  searchQuery,
  t,
}: {
  canUpload: boolean;
  searchQuery: string;
  t: TFunction;
}) {
  return (
    <WorkbenchEmptyState
      icon={<FileText className="h-7 w-7 text-muted-foreground" />}
      title={searchQuery ? t("documents.noMatchingDocs") : t("documents.noDocs")}
      description={
        searchQuery
          ? t("documents.noMatchingDocsDesc")
          : t("documents.noDocsDesc")
      }
      action={
        !searchQuery && canUpload ? (
          <div className="flex flex-col items-center gap-2">
            <Upload className="h-6 w-6 text-primary/70" />
            <p className="text-sm font-semibold text-foreground">
              {t("documents.dropZoneTitle")}
            </p>
            <p className="text-xs text-muted-foreground">
              {t("documents.dropZoneHint")}
            </p>
          </div>
        ) : undefined
      }
    />
  );
}
