import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import {
  Activity,
  ArrowLeft,
  ArrowRight,
  Brain,
  CheckCircle2,
  Database,
  Download,
  HardDriveDownload,
  ListChecks,
  Terminal,
  Upload,
} from 'lucide-react';
import { toast } from 'sonner';

import { adminApi, queries } from '@/shared/api';
import { Button } from '@/shared/components/ui/button';
import { Checkbox } from '@/shared/components/ui/checkbox';
import { DataState } from '@/shared/components/DataState';
import { useApp } from '@/shared/contexts/app-context';
import { errorMessage } from '@/shared/lib/errorMessage';
import { OperationsTab } from './OperationsTab';
import { McpConnectGuide } from './McpTab';
import { BackupExportDialog, BackupImportDialog } from './BackupDialogs';

/** Internal Library Hub sections, persisted in `?section=`. */
const HUB_SECTIONS = ['overview', 'activity', 'backup', 'mcp'] as const;
type HubSection = (typeof HUB_SECTIONS)[number];

function parseSection(value: string | null): HubSection {
  return HUB_SECTIONS.includes(value as HubSection) ? (value as HubSection) : 'overview';
}

const SECTION_ICONS: Record<HubSection, typeof Activity> = {
  overview: Activity,
  activity: ListChecks,
  backup: HardDriveDownload,
  mcp: Terminal,
};

/**
 * Library Hub (ADM-02) — the structural keystone of the admin restructure.
 *
 * A single library used to be administered across Admin → Operations
 * (health/audit/backup), Admin → Ingest Queue (jobs), Admin → MCP (the
 * doc-hint toggle), and the Libraries inspector. This is the one coherent,
 * bookmarkable per-library detail route (`/admin/library/:libraryId`) that
 * absorbs all of them, with internal sections and a "Configure AI →"
 * deep-link to this library's bindings.
 */
export default function LibraryHubPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { libraryId } = useParams<{ libraryId: string }>();
  const [searchParams, setSearchParams] = useSearchParams();
  const { activeWorkspace, activeLibrary, libraries, selectWorkspaceLibrary } = useApp();

  const section = parseSection(searchParams.get('section'));
  const [exportOpen, setExportOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);

  // The Hub is a true per-library route: when the URL targets a library that
  // is not the app-level active one, switch scope to it so every scoped query
  // (ops, audit, MCP prompt) below reads the route's library, not a stale
  // shell selection. We resolve the owning workspace from the local catalog.
  const routedLibrary = useMemo(
    () => libraries.find((lib) => lib.id === libraryId) ?? null,
    [libraries, libraryId],
  );
  useEffect(() => {
    if (!libraryId) return;
    if (activeLibrary?.id === libraryId) return;
    if (routedLibrary) {
      selectWorkspaceLibrary(routedLibrary.workspaceId, routedLibrary.id);
    }
  }, [libraryId, activeLibrary?.id, routedLibrary, selectWorkspaceLibrary]);

  const catalogQuery = useQuery({
    ...queries.getCatalogLibraryOptions({ path: { libraryId: libraryId ?? '' } }),
    enabled: Boolean(libraryId),
  });

  const libraryName =
    catalogQuery.data?.displayName ?? routedLibrary?.name ?? activeLibrary?.name ?? libraryId ?? '';
  const readiness = catalogQuery.data?.ingestionReadiness;
  const isReady = readiness?.ready ?? false;
  const missingPurposes = readiness?.missingBindingPurposes ?? [];

  const setSection = (next: HubSection) => {
    const params = new URLSearchParams(searchParams);
    params.set('section', next);
    setSearchParams(params, { replace: true });
  };

  const goToAi = () => {
    void navigate(`/admin/ai?scope=library&lib=${libraryId ?? ''}&section=bindings`);
  };

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      {/* Hub header — back link, name, readiness, Configure AI deep-link. */}
      <div className="shrink-0 border-b bg-gradient-to-b from-card/60 to-transparent px-6 py-4">
        <button
          type="button"
          onClick={() => navigate('/admin/libraries')}
          className="mb-2 inline-flex items-center gap-1.5 text-xs font-semibold text-muted-foreground transition-colors hover:text-foreground"
        >
          <ArrowLeft className="h-3.5 w-3.5" />
          {t('admin.libraryHub.backToCatalog')}
        </button>
        <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
          <div className="flex min-w-0 items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-surface-sunken">
              <Database className="h-5 w-5 text-muted-foreground" />
            </div>
            <div className="min-w-0">
              <h2 className="truncate text-lg font-bold tracking-tight" title={libraryName}>
                {libraryName}
              </h2>
              <p className="truncate text-xs text-muted-foreground">
                {activeWorkspace?.name ?? ''}
              </p>
            </div>
            <span
              className={`status-badge shrink-0 text-[10px] ${isReady ? 'status-ready' : 'status-warning'}`}
            >
              {isReady ? t('admin.libraryHub.ready') : t('admin.libraryHub.blocked')}
            </span>
          </div>
          <Button size="sm" variant={isReady ? 'outline' : 'default'} onClick={goToAi}>
            <Brain className="mr-1.5 h-3.5 w-3.5" />
            {t('admin.libraryHub.configureAi')}
            <ArrowRight className="ml-1.5 h-3.5 w-3.5" />
          </Button>
        </div>

        {/* Section switcher */}
        <div className="mt-4 flex gap-1 overflow-x-auto">
          {HUB_SECTIONS.map((value) => {
            const Icon = SECTION_ICONS[value];
            const active = section === value;
            return (
              <button
                key={value}
                type="button"
                onClick={() => setSection(value)}
                aria-current={active ? 'page' : undefined}
                className={`flex shrink-0 items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition-colors ${
                  active
                    ? 'bg-primary text-primary-foreground shadow-sm'
                    : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'
                }`}
              >
                <Icon className="h-3.5 w-3.5" />
                {t(`admin.libraryHub.sections.${value}`)}
              </button>
            );
          })}
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-auto p-6">
        {!libraryId ? (
          <div className="rounded-xl border bg-surface-sunken p-8 text-center text-sm text-muted-foreground">
            {t('admin.libraryHub.noLibrary')}
          </div>
        ) : section === 'overview' ? (
          <LibraryHubOverview
            isReady={isReady}
            missingCount={missingPurposes.length}
            onConfigureAi={goToAi}
          />
        ) : section === 'activity' ? (
          <LibraryHubActivity
            workspaceId={activeWorkspace?.id}
            libraryId={libraryId}
          />
        ) : section === 'backup' ? (
          <LibraryHubBackup
            libraryId={libraryId}
            onExport={() => setExportOpen(true)}
            onImport={() => setImportOpen(true)}
          />
        ) : (
          <LibraryHubMcp
            libraryId={libraryId}
            serverIncludeDocumentHintInMcpAnswers={
              catalogQuery.data?.includeDocumentHintInMcpAnswers
            }
          />
        )}
      </div>

      {libraryId && (
        <>
          <BackupExportDialog open={exportOpen} onOpenChange={setExportOpen} libraryId={libraryId} t={t} />
          <BackupImportDialog
            open={importOpen}
            onOpenChange={setImportOpen}
            libraryId={libraryId}
            t={t}
            onCompleted={() => void catalogQuery.refetch()}
          />
        </>
      )}
    </div>
  );
}

/**
 * Activity section — folds the old standalone Operations tab into the Hub for
 * per-library health and audit. Cross-library queue control lives at
 * `/admin/queue`.
 */
function LibraryHubActivity({
  workspaceId,
  libraryId,
}: {
  workspaceId: string | undefined;
  libraryId: string;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <OperationsTab t={t} activeWorkspaceId={workspaceId} activeLibraryId={libraryId} active />
    </div>
  );
}

function LibraryHubOverview({
  isReady,
  missingCount,
  onConfigureAi,
}: {
  isReady: boolean;
  missingCount: number;
  onConfigureAi: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="space-y-4">
      <div
        className={`flex items-start gap-3 rounded-xl border p-4 ${
          isReady
            ? 'border-status-ready/20 bg-status-ready/5'
            : 'border-status-warning/25 bg-status-warning/5'
        }`}
      >
        <div
          className={`mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl ${
            isReady ? 'bg-status-ready-bg text-status-ready' : 'bg-status-warning-bg text-status-warning'
          }`}
        >
          <CheckCircle2 className="h-4 w-4" />
        </div>
        <div className="min-w-0 flex-1">
          <h3 className="text-sm font-bold tracking-tight">
            {isReady
              ? t('admin.libraryHub.overview.readyTitle')
              : t('admin.libraryHub.overview.blockedTitle', { count: missingCount })}
          </h3>
          <p className="mt-1 text-sm text-muted-foreground">
            {isReady
              ? t('admin.libraryHub.overview.readyDesc')
              : t('admin.libraryHub.overview.blockedDesc')}
          </p>
          {!isReady && (
            <Button size="sm" className="mt-3" onClick={onConfigureAi}>
              <Brain className="mr-1.5 h-3.5 w-3.5" />
              {t('admin.libraryHub.configureAi')}
            </Button>
          )}
        </div>
      </div>
      <p className="text-xs text-muted-foreground">{t('admin.libraryHub.overview.hint')}</p>
    </div>
  );
}

function LibraryHubBackup({
  onExport,
  onImport,
}: {
  libraryId: string;
  onExport: () => void;
  onImport: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="max-w-2xl space-y-4">
      <div>
        <h3 className="text-base font-bold tracking-tight">{t('admin.libraryHub.backup.title')}</h3>
        <p className="mt-1 text-sm text-muted-foreground">{t('admin.libraryHub.backup.desc')}</p>
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <button
          type="button"
          onClick={onExport}
          className="workbench-surface flex flex-col items-start gap-2 p-4 text-left transition-shadow hover:shadow-lifted"
        >
          <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-surface-sunken">
            <Download className="h-4 w-4 text-muted-foreground" />
          </div>
          <div>
            <div className="text-sm font-bold">{t('admin.snapshot.export')}</div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {t('admin.libraryHub.backup.exportDesc')}
            </p>
          </div>
        </button>
        <button
          type="button"
          onClick={onImport}
          className="workbench-surface flex flex-col items-start gap-2 p-4 text-left transition-shadow hover:shadow-lifted"
        >
          <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-surface-sunken">
            <Upload className="h-4 w-4 text-muted-foreground" />
          </div>
          <div>
            <div className="text-sm font-bold">{t('admin.snapshot.import')}</div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {t('admin.libraryHub.backup.importDesc')}
            </p>
          </div>
        </button>
      </div>
    </div>
  );
}

function LibraryHubMcp({
  libraryId,
  serverIncludeDocumentHintInMcpAnswers,
}: {
  libraryId: string;
  serverIncludeDocumentHintInMcpAnswers?: boolean;
}) {
  const { t } = useTranslation();
  const {
    activeLibrary,
    refreshSession,
    libraries,
    setActiveLibrary,
    setLibraries,
  } = useApp();
  const library = libraries.find((lib) => lib.id === libraryId) ?? null;
  const [localCheckedOverride, setLocalCheckedOverride] = useState<{
    libraryId: string;
    value: boolean;
  } | null>(null);
  const localChecked =
    localCheckedOverride?.libraryId === libraryId ? localCheckedOverride.value : null;

  // The MCP document-hint toggle (RM-06) is a live, library-scoped setting.
  // It used to be buried inside the MCP docs tab with no clear scope; here it
  // lives next to the library it configures.
  const mcpSettingsMutation = useMutation({
    mutationFn: (includeDocumentHintInMcpAnswers: boolean) =>
      adminApi.updateLibraryMcpSettings(libraryId, { includeDocumentHintInMcpAnswers }),
    onMutate: (includeDocumentHintInMcpAnswers) => {
      const previous = localChecked ?? library?.includeDocumentHintInMcpAnswers ?? null;
      setLocalCheckedOverride({ libraryId, value: includeDocumentHintInMcpAnswers });
      return { previous };
    },
    onSuccess: (updatedLibrary, requestedValue) => {
      const includeDocumentHintInMcpAnswers =
        updatedLibrary.includeDocumentHintInMcpAnswers ?? requestedValue;
      setLocalCheckedOverride({ libraryId, value: includeDocumentHintInMcpAnswers });
      setLibraries((previous) =>
        previous.map((item) =>
          item.id === libraryId ? { ...item, includeDocumentHintInMcpAnswers } : item,
        ),
      );
      if (activeLibrary?.id === libraryId) {
        setActiveLibrary({ ...activeLibrary, includeDocumentHintInMcpAnswers });
      }
      void Promise.resolve(refreshSession()).catch(() => undefined);
    },
    onError: (error: unknown, _requestedValue, context) => {
      setLocalCheckedOverride(
        context?.previous == null ? null : { libraryId, value: context.previous },
      );
      toast.error(errorMessage(error, t('admin.mcp.updateFailed')));
    },
  });
  const hasLoadedLibrary = Boolean(library) || serverIncludeDocumentHintInMcpAnswers != null;
  const checked =
    localChecked ?? library?.includeDocumentHintInMcpAnswers
    ?? serverIncludeDocumentHintInMcpAnswers
    ?? true;

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-base font-bold tracking-tight">{t('admin.libraryHub.mcp.title')}</h3>
        <p className="mt-1 text-sm text-muted-foreground">{t('admin.libraryHub.mcp.desc')}</p>
      </div>

      <div className="workbench-surface p-4">
        <label className="flex items-start gap-3">
          <Checkbox
            checked={checked}
            disabled={!hasLoadedLibrary || mcpSettingsMutation.isPending}
            onCheckedChange={(value) => mcpSettingsMutation.mutate(value === true)}
          />
          <span className="min-w-0">
            <span className="block text-sm font-semibold">{t('admin.mcp.includeDocumentHint')}</span>
            <span className="mt-1 block text-xs text-muted-foreground">
              {t('admin.mcp.includeDocumentHintHelp')}
            </span>
          </span>
        </label>
      </div>

      <DataState
        query={{
          isLoading: false,
          error: null,
          data: libraryId,
        }}
      >
        {() => <McpConnectGuide t={t} libraryId={libraryId} />}
      </DataState>
    </div>
  );
}
