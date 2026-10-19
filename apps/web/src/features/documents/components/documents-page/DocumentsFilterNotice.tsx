import { AlertTriangle, Info } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'

import type { IndexingFilterBucket } from './documentsPageState'

type Props = {
  bucket: IndexingFilterBucket
  /** Documents on the loaded page. There is no known total for these buckets. */
  visibleCount: number
  /** Visible documents the server will accept a reprocess for right now. */
  reprocessableCount: number
  canWrite: boolean
  onSelectReprocessable: () => void
}

/// A ready document can still lack a search index: no chunk vectors, or no
/// extracted entities and relations. Both look like an ordinary `ready` row, so
/// a filter that selects them states nothing on its own — this names what the
/// active filter means and, when the reader may write, offers the one action
/// that rebuilds the missing index. Deleted and in-flight documents are not
/// reprocessable and are excluded by `reprocessableCount`.
const NOTICE_KEYS: Record<IndexingFilterBucket, { title: string; detail: string; gap: boolean }> = {
  vectorgap: {
    title: 'documents.filterNotice.vectorGapTitle',
    detail: 'documents.filterNotice.vectorGapDetail',
    gap: true,
  },
  graphgap: {
    title: 'documents.filterNotice.graphGapTitle',
    detail: 'documents.filterNotice.graphGapDetail',
    gap: true,
  },
  graphready: {
    title: 'documents.filterNotice.graphReadyTitle',
    detail: 'documents.filterNotice.graphReadyDetail',
    gap: false,
  },
}

export function DocumentsFilterNotice({
  bucket,
  visibleCount,
  reprocessableCount,
  canWrite,
  onSelectReprocessable,
}: Readonly<Props>) {
  const { t } = useTranslation()
  const notice = NOTICE_KEYS[bucket]
  return (
    <div className="documents-filter-notice" role="note">
      {notice.gap ? <AlertTriangle size={18} /> : <Info size={18} />}
      <div className="documents-filter-notice__body">
        <strong>{t(notice.title)}</strong>
        <p>{t(notice.detail)}</p>
        <small>{t('documents.filterNotice.pageCount', { count: visibleCount })}</small>
      </div>
      {canWrite && notice.gap && (
        <div className="documents-filter-notice__action">
          {reprocessableCount > 0 ? (
            <Button size="sm" onClick={onSelectReprocessable}>
              {t('documents.filterNotice.selectReprocessable', { count: reprocessableCount })}
            </Button>
          ) : (
            <small>{t('documents.filterNotice.nothingReprocessable')}</small>
          )}
        </div>
      )}
    </div>
  )
}
