import type { TFunction } from 'i18next'
import { RotateCw, Trash2, XCircle } from 'lucide-react'

import { Button } from '@/shared/components/ui/button'

type BulkSelectionBarProps = Readonly<{
  clearSelection: () => void
  onBulkCancel: () => void
  onBulkDelete: () => void
  onBulkReprocess: () => void
  selectedCount: number
  t: TFunction
}>

export function BulkSelectionBar({
  clearSelection,
  onBulkCancel,
  onBulkDelete,
  onBulkReprocess,
  selectedCount,
  t,
}: BulkSelectionBarProps) {
  if (selectedCount <= 0) return null
  return (
    <div className="sticky bottom-0 z-10 flex items-center gap-3 border-t bg-background px-4 py-3 shadow-lg">
      <span className="text-sm font-medium tabular-nums">
        {t('documents.nSelected', { count: selectedCount })}
      </span>
      <Button variant="destructive" size="sm" onClick={onBulkDelete}>
        <Trash2 className="h-3.5 w-3.5 mr-1.5" />
        {t('documents.deleteSelected')}
      </Button>
      <Button variant="outline" size="sm" onClick={onBulkCancel}>
        <XCircle className="h-3.5 w-3.5 mr-1.5" />
        {t('documents.cancelProcessing')}
      </Button>
      <Button variant="outline" size="sm" onClick={onBulkReprocess}>
        <RotateCw className="h-3.5 w-3.5 mr-1.5" />
        {t('documents.retrySelected')}
      </Button>
      <div className="flex-1" />
      <Button variant="ghost" size="sm" onClick={clearSelection}>
        {t('documents.clearSelection')}
      </Button>
    </div>
  )
}
