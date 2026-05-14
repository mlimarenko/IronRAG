import type { TFunction } from "i18next";
import type { Dispatch, SetStateAction } from "react";
import { FileText, Loader2, RotateCw, Upload, XCircle } from "lucide-react";

import type {
  DocumentListPageResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentListStatusFilter,
} from "@/shared/api";
import { Button } from "@/shared/components/ui/button";
import { DataState } from "@/shared/components/DataState";
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

  return (
    <>
      <DocumentsFiltersBar
        libraryCost={props.libraryCost}
        onCancelSelection={list.clearSelection}
        onStartSelection={() => list.setSelectionMode(true)}
        searchQuery={props.searchQuery}
        selectionMode={list.selectionMode}
        statusBucket={props.statusBucket}
        statusCounts={props.statusCounts ?? null}
        t={props.t}
        updateSearchParamState={props.updateSearchParamState}
        workspaceCost={props.workspaceCost}
      />
      <div
        className={`flex-1 min-w-0 overflow-hidden ${props.uploadController.dragOver ? "ring-2 ring-primary ring-inset bg-primary/5" : ""}`}
        {...props.uploadController.dropTargetProps}
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
        {props.uploadController.dragOver && <DropOverlay t={props.t} />}
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
          emptyRender={<EmptyState searchQuery={props.searchQuery} t={props.t} />}
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
                  sortBy={props.sortBy}
                  sortOrder={props.sortOrder}
                  t={props.t}
                />
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
            <Loader2 className="h-3 w-3 mr-1.5 animate-spin" />
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
      <div className="p-8 rounded-2xl border-2 border-dashed border-primary bg-card shadow-elevated">
        <Upload className="h-8 w-8 text-primary mx-auto mb-3" />
        <p className="text-sm font-bold">{t("documents.dropToUpload")}</p>
      </div>
    </div>
  );
}

function LoadingState({ t }: { t: TFunction }) {
  return (
    <div className="empty-state py-20">
      <Loader2 className="h-7 w-7 animate-spin text-primary mb-4" />
      <h2 className="text-base font-bold tracking-tight">
        {t("documents.loadingDocs")}
      </h2>
    </div>
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
    <div className="empty-state py-20">
      <div className="w-14 h-14 rounded-2xl bg-destructive/10 flex items-center justify-center mb-4">
        <XCircle className="h-7 w-7 text-destructive" />
      </div>
      <h2 className="text-base font-bold tracking-tight">
        {t("documents.failedToLoad")}
      </h2>
      <p className="text-sm text-muted-foreground mt-2">{error}</p>
      <Button size="sm" variant="outline" className="mt-4" onClick={onRetry}>
        <RotateCw className="h-3.5 w-3.5 mr-1.5" />
        {t("documents.retry")}
      </Button>
    </div>
  );
}

function EmptyState({
  searchQuery,
  t,
}: {
  searchQuery: string;
  t: TFunction;
}) {
  return (
    <div className="empty-state py-20">
      <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
        <FileText className="h-7 w-7 text-muted-foreground" />
      </div>
      <h2 className="text-base font-bold tracking-tight">
        {searchQuery ? t("documents.noMatchingDocs") : t("documents.noDocs")}
      </h2>
      <p className="text-sm text-muted-foreground mt-2">
        {searchQuery
          ? t("documents.noMatchingDocsDesc")
          : t("documents.noDocsDesc")}
      </p>
    </div>
  );
}
