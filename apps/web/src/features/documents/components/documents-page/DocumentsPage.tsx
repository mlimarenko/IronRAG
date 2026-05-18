import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { useApp } from "@/shared/contexts/app-context";

import { DocumentsPageHeader } from "@/features/documents/components/DocumentsPageHeader";

import { DocumentsListSection } from "./DocumentsListSection";
import { InspectorSection } from "./InspectorSection";
import { NoLibraryState } from "./NoLibraryState";
import { UploadQueueSection } from "./UploadQueueSection";
import { useUploadQueueController } from "./useUploadQueueController";
import { WebIngestSection } from "./WebIngestSection";
import {
  useDocumentsPageUrlState,
  useDocumentsTableState,
  type DocumentsPageTab,
} from "./documentsPageState";
import { useDocumentsQueries } from "./useDocumentsQueries";
import { useWebIngestController } from "./useWebIngestController";

export function DocumentsPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { activeLibrary, activeWorkspace, locale, user } = useApp();
  const [tableState, setTableState] = useDocumentsTableState();
  const urlState = useDocumentsPageUrlState({ tableState, setTableState });
  const [activeTab, setActiveTab] = useState<DocumentsPageTab>("documents");
  const [selectionMode, setSelectionMode] = useState(false);
  const documents = useDocumentsQueries({
    activeLibrary,
    activeWorkspace,
    pageSize: urlState.pageSize,
    searchQuery: urlState.searchQuery,
    selectedDocumentId: urlState.selectedDocumentId,
    sortValue: urlState.sortValue,
    statusBackendFilter: urlState.statusBackendFilter,
    statusBucket: urlState.statusBucket,
    t,
    updateSearchParamState: urlState.updateSearchParamState,
  });
  const uploadQueue = useUploadQueueController({
    activeLibrary,
    activateListPollGrace: documents.activateListPollGrace,
    errorMessage: documents.errorMessage,
    items: documents.items,
    loadFirstPage: documents.loadFirstPage,
    t,
  });
  const webIngest = useWebIngestController({
    activeLibrary,
    errorMessage: documents.errorMessage,
    fetchLibraryWebIngestPolicy: documents.fetchLibraryWebIngestPolicy,
    libraryPolicyData: documents.libraryPolicyQuery.data,
    libraryPolicyLoading: documents.libraryPolicyQuery.isLoading,
    loadedWebIngestPolicy: documents.loadedWebIngestPolicy,
    loadFirstPage: documents.loadFirstPage,
    refreshWebRuns: documents.refreshWebRuns,
    t,
    webRuns: documents.webRuns,
    webRunsRefreshing: documents.webRunsRefreshing,
  });
  const documentHintEditable = user?.role === "admin" || user?.role === "operator";

  if (!activeLibrary) return <NoLibraryState t={t} />;

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <DocumentsPageHeader
        activeLibraryName={activeLibrary.name}
        activeTab={activeTab} documentsCount={documents.totalCount ?? documents.items.length}
        fileInputRef={uploadQueue.fileInputRef} folderInputRef={uploadQueue.folderInputRef}
        handleFileSelect={uploadQueue.handleFileSelect}
        handleFolderSelect={uploadQueue.handleFolderSelect}
        onRefreshWebRuns={() => void webIngest.refreshWebRuns()}
        setActiveTab={setActiveTab} setAddLinkOpen={webIngest.setAddLinkOpen}
        setBoundaryPolicy={webIngest.setBoundaryPolicy} setCrawlMode={webIngest.setCrawlMode}
        setMaxDepth={webIngest.setMaxDepth} setMaxPages={webIngest.setMaxPages}
        setSeedUrl={webIngest.setSeedUrl} t={t}
        webRunsCount={webIngest.webRuns.length} webRunsRefreshing={webIngest.webRunsRefreshing}
        ingestionReady={activeLibrary.ingestionReady}
        onOpenAiSettings={() => navigate('/admin?tab=ai')}
      />
      <div className="flex-1 flex overflow-hidden">
        {activeTab === "documents" ? (
          <div className="flex flex-1 min-w-0 flex-col">
            <DocumentsListSection
              activeLibrary={activeLibrary}
              activateListPollGrace={documents.activateListPollGrace}
              debouncedSearch={documents.debouncedSearch} errorMessage={documents.errorMessage}
              filteredTotal={documents.filteredTotal} isLoading={documents.isLoading}
              items={documents.items} libraryCost={documents.libraryCost}
              loadError={documents.loadError} loadFirstPage={documents.loadFirstPage} locale={locale}
              localSort={tableState.localSort}
              onSelectionModeChange={setSelectionMode}
              pageSize={urlState.pageSize} pagination={documents.pagination}
              pendingUploads={uploadQueue.pendingUploads} searchQuery={urlState.searchQuery}
              selectedDoc={documents.selectedDoc} selectDoc={documents.selectDoc}
              sortBy={documents.sortBy} sortOrder={documents.sortOrder} sortValue={urlState.sortValue}
              statusBackendFilter={urlState.statusBackendFilter} statusBucket={urlState.statusBucket}
              statusCounts={documents.statusCounts} t={t}
              setTableState={setTableState}
              updateSearchParamState={urlState.updateSearchParamState}
              uploadController={uploadQueue} workspaceCost={documents.workspaceCost}
            />
          </div>
        ) : (
          <div className="flex-1 min-w-0 overflow-hidden">
            <WebIngestSection controller={webIngest} t={t} />
          </div>
        )}
        <InspectorSection
          activateListPollGrace={documents.activateListPollGrace}
          clearSelectedDoc={documents.clearSelectedDoc} errorMessage={documents.errorMessage}
          documentHintEditable={documentHintEditable}
          fetchSelectedDetail={documents.fetchSelectedDetail}
          inspectorLifecycle={documents.inspectorLifecycle} loadFirstPage={documents.loadFirstPage}
          locale={locale} selectedDoc={documents.selectedDoc}
          selectionMode={selectionMode} selectDoc={documents.selectDoc} t={t}
          updateDocumentHintLocally={documents.updateDocumentHintLocally}
          updateSearchParamState={urlState.updateSearchParamState}
        />
      </div>
      <UploadQueueSection controller={uploadQueue} t={t} />
    </div>
  );
}
