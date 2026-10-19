import { useTranslation } from 'react-i18next'
import { Link } from 'react-router-dom'

import type { AiBindingPurpose } from '@/shared/api/generated'
import { purposeLabel } from '@/features/admin/model/aiConfig'

import type { AdminLibraryRow } from '../model/librariesAdapter'

const VISIBLE_PURPOSE_LABEL_COUNT = 2

/** The ingestion bindings a blocked library is missing, capped for the cell. */
export function MissingBindingPurposeLabels({ purposes }: Readonly<{ purposes: string[] }>) {
  const { t } = useTranslation()
  const labels = purposes.map((purpose) => purposeLabel(purpose as AiBindingPurpose, t))
  const hiddenLabelCount = Math.max(0, labels.length - VISIBLE_PURPOSE_LABEL_COUNT)
  return (
    <small title={labels.join(', ')}>
      {labels.slice(0, VISIBLE_PURPOSE_LABEL_COUNT).join(', ')}
      {hiddenLabelCount > 0 && ` +${hiddenLabelCount}`}
    </small>
  )
}

/** Nonzero queued counts open the library queue; zero remains plain text. */
export function LibraryQueueLink({ row }: Readonly<{ row: AdminLibraryRow }>) {
  const { t, i18n } = useTranslation()
  const count = new Intl.NumberFormat(i18n.language).format(row.queuedJobCount)
  if (row.queuedJobCount === 0) return <span>{count}</span>
  return (
    <Link
      className="table-link"
      to={`/admin/queue?libraryId=${encodeURIComponent(row.id)}&status=queued`}
      aria-label={t('admin.libraries.openQueue', {
        name: row.displayName,
        count: row.queuedJobCount,
      })}
    >
      {count}
    </Link>
  )
}
