import type { TFunction } from 'i18next'
import { FileText } from 'lucide-react'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState'

export function NoLibraryState({ t }: Readonly<{ t: TFunction }>) {
  return (
    <PageShell header={<PageHeader title={t('documents.title')} />} bodyClassName="empty-state">
      <WorkbenchEmptyState
        icon={<FileText className="h-7 w-7 text-muted-foreground" />}
        title={t('documents.noLibrary')}
        description={t('documents.noLibraryDesc')}
      />
    </PageShell>
  )
}
