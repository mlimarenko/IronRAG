import { FilePlus2, Link2, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'

import type { DocumentsPageTab } from './documentsPageState'

/**
 * The documents page header buttons. Which of the four appear depends on the
 * open tab and on two separate permissions, and holding that decision inside
 * the page put a third of its branching into one JSX attribute.
 */
export function DocumentsPageActions({
  canIngest,
  canUpload,
  ingestionReady,
  onAddLink,
  onPickFiles,
  onRefreshDocuments,
  onRefreshWebRuns,
  tab,
  webRunsRefreshing,
}: Readonly<{
  canIngest: boolean
  canUpload: boolean
  ingestionReady: boolean
  onAddLink: () => void
  onPickFiles: () => void
  onRefreshDocuments: () => void
  onRefreshWebRuns: () => void
  tab: DocumentsPageTab
  webRunsRefreshing: boolean
}>) {
  const { t } = useTranslation()
  const onDocuments = tab === 'documents'
  return (
    <>
      {onDocuments && (
        <Button onClick={onRefreshDocuments}>
          <RefreshCw size={15} />
          {t('common.refresh')}
        </Button>
      )}
      {!onDocuments && (
        <Button onClick={onRefreshWebRuns} disabled={webRunsRefreshing}>
          <RefreshCw className={webRunsRefreshing ? 'spin' : ''} size={15} />
          {t('documents.refreshRuns')}
        </Button>
      )}
      {canUpload && onDocuments && (
        <Button variant="primary" disabled={!ingestionReady} onClick={onPickFiles}>
          <FilePlus2 size={15} />
          {t('documents.addContent')}
        </Button>
      )}
      {canIngest && !onDocuments && (
        <Button variant="primary" disabled={!ingestionReady} onClick={onAddLink}>
          <Link2 size={15} />
          {t('documents.addLink')}
        </Button>
      )}
    </>
  )
}
