import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import {
  AlertTriangle,
  FileText,
  MessageSquare,
  type LucideIcon,
} from 'lucide-react';
import { Button } from '@/shared/components/ui/button';

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
    <div className="flex-1 flex flex-col">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">{t('assistant.title')}</h1>
      </div>
      <div className="empty-state flex-1">
        <div
          className="w-14 h-14 rounded-2xl flex items-center justify-center mb-4"
          style={
            warning
              ? {
                  background: 'hsl(var(--status-warning-bg))',
                  boxShadow:
                    'inset 0 0 0 1px hsl(var(--status-warning-ring) / 0.3)',
                }
              : undefined
          }
        >
          <Icon
            className={`h-7 w-7 ${warning ? 'text-status-warning' : 'text-muted-foreground'}`}
          />
        </div>
        <h2 className="text-base font-bold tracking-tight">{title}</h2>
        <p className="text-sm text-muted-foreground mt-2 max-w-sm">
          {description}
        </p>
        {action}
      </div>
    </div>
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
