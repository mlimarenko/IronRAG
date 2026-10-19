import { AlertTriangle, FilePlus2, Settings } from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useTranslation } from 'react-i18next'

import { ChooseLibraryButton } from '@/app/ChooseLibraryButton'
import { useCan } from '@/shared/auth/useCan'
import { useApp } from '@/shared/contexts/app-context'
import { Badge, Button, PageHeader, Panel, ScreenState, Tabs } from '@/shared/ui'
import type { Library, Locale, Workspace } from '@/shared/types'

import { DocumentInspector } from './DocumentInspector'
import { DocumentList } from './DocumentList'
import { UploadDialog } from './UploadDialog'
import { WebIngestPanel } from './WebIngestPanel'
import { DocumentsPageActions } from './components/documents-page/DocumentsPageActions'
import {
  hasRecordedSpend,
  isLibraryEmpty,
  useDocumentsPageUrlState,
  useDocumentsTableState,
  type DocumentsPageTab,
} from './components/documents-page/documentsPageState'
import { useDocumentsListController } from './components/documents-page/useDocumentsListController'
import { useDocumentsQueries } from './components/documents-page/useDocumentsQueries'
import { useUploadQueueController } from './components/documents-page/useUploadQueueController'
import { useWebIngestController } from './components/documents-page/useWebIngestController'

const TERMINAL_RUN_STATES = new Set(['completed', 'completed_partial', 'failed', 'canceled'])

export default function DocumentsPage() {
  const { activeLibrary, activeWorkspace, locale } = useApp()
  const [tableState, setTableState] = useDocumentsTableState()
  const url = useDocumentsPageUrlState({
    libraryId: activeLibrary?.id ?? null,
    tableState,
    setTableState,
  })
  if (!activeLibrary) return <NoLibrary />
  return (
    <DocumentsLibraryPage
      key={activeLibrary.id}
      activeLibrary={activeLibrary}
      activeWorkspace={activeWorkspace}
      locale={locale}
      tableState={tableState}
      setTableState={setTableState}
      url={url}
    />
  )
}

function NoLibrary() {
  const { t } = useTranslation()
  return (
    <>
      <PageHeader title={t('documents.title')} />
      <Panel className="documents-empty">
        <strong>{t('documents.noLibrary')}</strong>
        <p>{t('documents.noLibraryDesc')}</p>
        <ChooseLibraryButton />
      </Panel>
    </>
  )
}

function DocumentsLibraryPage({
  activeLibrary,
  activeWorkspace,
  locale,
  tableState,
  setTableState,
  url,
}: Readonly<{
  activeLibrary: Library
  activeWorkspace: Workspace | null
  locale: Locale
  tableState: ReturnType<typeof useDocumentsTableState>[0]
  setTableState: ReturnType<typeof useDocumentsTableState>[1]
  url: ReturnType<typeof useDocumentsPageUrlState>
}>) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { can } = useCan()
  const [tab, setTab] = useState<DocumentsPageTab>('documents')
  const documents = useDocumentsQueries({
    activeLibrary,
    activeWorkspace,
    locale,
    pageSize: url.pageSize,
    searchQuery: url.searchQuery,
    selectedDocumentId: url.selectedDocumentId,
    sortValue: url.sortValue,
    listFilter: url.listFilter,
    statusBucket: url.statusBucket,
    columnFilters: url.columnFilters,
    filtersKey: url.filtersKey,
    snapshotAt: url.snapshotAt,
    t,
    updateSearchParamState: url.updateSearchParamState,
  })
  const uploads = useUploadQueueController({
    activeLibrary,
    activateListPollGrace: documents.activateListPollGrace,
    errorMessage: documents.errorMessage,
    items: documents.items,
    loadFirstPage: documents.loadFirstPage,
    openExistingDocument: (documentId) => url.updateSearchParamState({ documentId }),
    t,
  })
  const web = useWebIngestController({
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
  })
  const list = useDocumentsListController({
    activeLibrary,
    activateListPollGrace: documents.activateListPollGrace,
    errorMessage: documents.errorMessage,
    filteredTotal: documents.filteredTotal,
    hasMorePages: documents.pagination.canGoNext,
    items: documents.items,
    loadFirstPage: documents.loadFirstPage,
    pageSize: url.pageSize,
    setTableState,
    sortBy: documents.sortBy,
    sortOrder: documents.sortOrder,
    sortValue: url.sortValue,
    listFilter: url.listFilter,
    columnFilters: url.columnFilters,
    search: documents.searchDerivation.search,
    searchStatus: documents.searchDerivation.searchStatus,
    normalizedSearch: documents.searchDerivation.normalizedSearch,
    searchRanges: documents.searchDerivation.searchRanges,
    snapshotAt: documents.snapshotAt,
    t,
    updateSearchParamState: url.updateSearchParamState,
  })
  const activeRuns = web.webRuns.filter((run) => !TERMINAL_RUN_STATES.has(run.runState))
  const canUpload = can('content.upload')
  const canIngest = can('content.ingest')
  const uploadReady = canUpload && activeLibrary.ingestionReady
  const canEdit = can('content.edit')
  const canDelete = can('library.delete')
  const hasExpenses = hasRecordedSpend({
    libraryCost: documents.libraryCost,
    workspaceCost: documents.workspaceCost,
    libraryUnpricedCalls: documents.libraryUnpricedCalls,
    workspaceUnpricedCalls: documents.workspaceUnpricedCalls,
  })
  const isEmptyLibrary = isLibraryEmpty({
    loading: documents.isLoading,
    loadFailed: Boolean(documents.loadError),
    searchQuery: url.searchQuery,
    statusBucket: url.statusBucket,
    totalCount: documents.totalCount,
    loadedCount: documents.items.length,
    pendingUploadCount: uploads.pendingUploads.length,
  })

  return (
    <div
      className={`documents-page ${documents.selectedDoc ? 'documents-page--inspector' : ''}`}
      {...(uploadReady ? uploads.dropTargetProps : {})}
    >
      {uploadReady && uploads.dragOver && (
        <div className="drop-overlay">
          <FilePlus2 size={34} />
          <strong>{t('documents.dropToUpload')}</strong>
        </div>
      )}
      <PageHeader
        title={t('documents.title')}
        actions={
          <DocumentsPageActions
            canIngest={canIngest}
            canUpload={canUpload}
            ingestionReady={activeLibrary.ingestionReady}
            onAddLink={web.openCreateDialog}
            onPickFiles={() => uploads.fileInputRef.current?.click()}
            onRefreshDocuments={() => void documents.loadFirstPage()}
            onRefreshWebRuns={() => void web.refreshWebRuns()}
            tab={tab}
            webRunsRefreshing={web.webRunsRefreshing}
          />
        }
      />
      {!activeLibrary.ingestionReady && (
        <div className="documents-readiness" role="alert">
          <AlertTriangle size={18} />
          <div>
            <strong>{t('documents.ingestionNotReady')}</strong>
            <p>{t('documents.ingestionNotReadyDetail')}</p>
          </div>
          <Button size="sm" onClick={() => navigate('/admin/ai')}>
            <Settings size={14} />
            {t('documents.ingestionNotReadyAction')}
          </Button>
        </div>
      )}
      <Tabs
        id="documents"
        label={t('nav.documents')}
        value={tab}
        onChange={setTab}
        items={[
          { value: 'documents', label: t('documents.tabs.documents') },
          {
            value: 'web',
            label: t('documents.tabs.webIngest'),
            suffix: (
              <Badge tone={activeRuns.length ? 'processing' : 'neutral'}>
                {activeRuns.length ? <span className="live-dot" /> : null}
                {web.webRuns.length}
              </Badge>
            ),
          },
        ]}
      >
        {tab === 'documents' ? (
          <ScreenState
            scenario={isEmptyLibrary ? 'empty' : 'ready'}
            emptyText={t('documents.noDocs')}
            action={
              canUpload ? (
                <Button
                  variant="primary"
                  disabled={!activeLibrary.ingestionReady}
                  onClick={() => uploads.fileInputRef.current?.click()}
                >
                  <FilePlus2 size={15} />
                  {t('dashboard.emptyAction')}
                </Button>
              ) : undefined
            }
          >
            <div className={hasExpenses ? undefined : '[&>.cost-strip]:hidden'}>
              <DocumentList
                canWrite={canEdit || canDelete}
                documents={documents}
                list={list}
                locale={locale}
                showDetailColumns={tableState.showDetailColumns}
                uploads={uploads}
                url={url}
              />
            </div>
          </ScreenState>
        ) : (
          <WebIngestPanel controller={web} canWrite={canIngest && activeLibrary.ingestionReady} />
        )}
      </Tabs>
      <DocumentInspector
        activateListPollGrace={documents.activateListPollGrace}
        canDelete={canDelete}
        canEdit={canEdit}
        document={documents.selectedDoc}
        errorMessage={documents.errorMessage}
        fetchSelectedDetail={documents.fetchSelectedDetail}
        lifecycle={documents.inspectorLifecycle}
        loadFirstPage={documents.loadFirstPage}
        locale={locale}
        onClose={documents.clearSelectedDoc}
        onDocumentHintUpdated={documents.updateDocumentHintLocally}
        onSelectDocument={documents.selectDoc}
      />
      <UploadDialog controller={uploads} />
    </div>
  )
}
