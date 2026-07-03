import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import {
  AlertTriangle,
  FileText,
  MessageSquare,
  type LucideIcon,
} from 'lucide-react';
import { Button } from '@/shared/components/ui/button';
import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState';

type EmptyStateProps = {
  t: TFunction;
  icon: LucideIcon;
  title: string;
  description: string;
  action: ReactNode;
  warning?: boolean;
};

function EmptyState({
  t,
  icon: Icon,
  title,
  description,
  action,
  warning = false,
}: EmptyStateProps) {
  return (
    <PageShell
      header={<PageHeader title={t('assistant.title')} />}
      bodyClassName="empty-state"
    >
        <WorkbenchEmptyState
          icon={
            <Icon
              className={`h-7 w-7 ${warning ? 'text-status-warning' : 'text-muted-foreground'}`}
            />
          }
          title={title}
          description={description}
          action={action}
        />
    </PageShell>
  );
}

export function NoLibraryState({
  t,
  onOpenDocuments,
}: {
  t: TFunction;
  onOpenDocuments: () => void;
}) {
  return (
    <EmptyState
      t={t}
      icon={MessageSquare}
      title={t('assistant.noLibrary')}
      description={t('assistant.noLibraryDesc')}
      action={
        <Button variant="outline" size="sm" className="mt-4" onClick={onOpenDocuments}>
          <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('assistant.goToDocuments')}
        </Button>
      }
    />
  );
}

export function QueryNotConfiguredState({
  t,
  onOpenAdmin,
}: {
  t: TFunction;
  onOpenAdmin: () => void;
}) {
  return (
    <EmptyState
      t={t}
      icon={AlertTriangle}
      title={t('assistant.queryNotConfigured')}
      description={t('assistant.queryNotConfiguredDesc')}
      warning
      action={
        <Button variant="outline" size="sm" className="mt-4" onClick={onOpenAdmin}>
          {t('assistant.goToAdmin')}
        </Button>
      }
    />
  );
}
