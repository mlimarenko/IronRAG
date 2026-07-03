import { useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { AlertTriangle, Download, Loader2, Upload } from 'lucide-react';

import { librarySnapshotApi } from '@/shared/api';
import type {
  LibrarySnapshotIncludeKind,
  LibrarySnapshotOverwriteMode,
} from '@/shared/api/documents';
import { Button } from '@/shared/components/ui/button';
import { Checkbox } from '@/shared/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { errorMessage } from '@/shared/lib/errorMessage';

type BackupDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  libraryId: string;
  t: TFunction;
};

export function BackupExportDialog({ open, onOpenChange, libraryId, t }: BackupDialogProps) {
  // Canonical library scope. Documents, runtime graph, knowledge base
  // and edges are always exported together — they describe a single
  // library and splitting them used to confuse operators. The only
  // real-world choice is whether to also bundle the original source
  // files (PDFs / docx / etc.), which can easily dwarf the rest of
  // the archive.
  const [includeBlobs, setIncludeBlobs] = useState(true);
  // Optional, portable extras the operator can fold into the archive: the
  // owning workspace row and the AI configuration (provider/model catalogs,
  // prices, presets, credentials without secrets, and binding assignments).
  const [includeWorkspace, setIncludeWorkspace] = useState(false);
  const [includeAiConfig, setIncludeAiConfig] = useState(false);

  const runExport = () => {
    const kinds: LibrarySnapshotIncludeKind[] = ['library_data'];
    if (includeBlobs) kinds.push('blobs');
    if (includeWorkspace) kinds.push('workspace');
    if (includeAiConfig) kinds.push('ai_config');
    librarySnapshotApi.downloadExport(libraryId, kinds);
    toast.success(t('admin.snapshot.exportSuccess'));
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('admin.snapshot.exportTitle')}</DialogTitle>
          <DialogDescription>{t('admin.snapshot.exportDesc')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div className="rounded-xl bg-surface-sunken p-3">
            <div className="text-sm font-semibold">{t('admin.snapshot.libraryDataLabel')}</div>
            <div className="text-xs text-muted-foreground mt-1">
              {t('admin.snapshot.libraryDataDesc')}
            </div>
          </div>
          <label
            htmlFor="backup-include-blobs"
            className="flex items-start gap-3 rounded-xl bg-surface-sunken p-3 cursor-pointer hover:bg-accent/30 transition-colors"
          >
            <Checkbox
              id="backup-include-blobs"
              checked={includeBlobs}
              onCheckedChange={(value) => setIncludeBlobs(value === true)}
              className="mt-0.5"
            />
            <div className="min-w-0">
              <div className="text-sm font-semibold">
                {t('admin.snapshot.includeBlobsLabel')}
              </div>
              <div className="text-xs text-muted-foreground mt-0.5">
                {t('admin.snapshot.includeBlobsDesc')}
              </div>
            </div>
          </label>
          <label
            htmlFor="backup-include-workspace"
            className="flex items-start gap-3 rounded-xl bg-surface-sunken p-3 cursor-pointer hover:bg-accent/30 transition-colors"
          >
            <Checkbox
              id="backup-include-workspace"
              checked={includeWorkspace}
              onCheckedChange={(value) => setIncludeWorkspace(value === true)}
              className="mt-0.5"
            />
            <div className="min-w-0">
              <div className="text-sm font-semibold">
                {t('admin.snapshot.includeWorkspaceLabel')}
              </div>
              <div className="text-xs text-muted-foreground mt-0.5">
                {t('admin.snapshot.includeWorkspaceDesc')}
              </div>
            </div>
          </label>
          <label
            htmlFor="backup-include-ai-config"
            className="flex items-start gap-3 rounded-xl bg-surface-sunken p-3 cursor-pointer hover:bg-accent/30 transition-colors"
          >
            <Checkbox
              id="backup-include-ai-config"
              checked={includeAiConfig}
              onCheckedChange={(value) => setIncludeAiConfig(value === true)}
              className="mt-0.5"
            />
            <div className="min-w-0">
              <div className="text-sm font-semibold">
                {t('admin.snapshot.includeAiConfigLabel')}
              </div>
              <div className="text-xs text-muted-foreground mt-0.5">
                {t('admin.snapshot.includeAiConfigDesc')}
              </div>
            </div>
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t('common.cancel')}
          </Button>
          <Button onClick={runExport}>
            <Download className="h-3.5 w-3.5 mr-1.5" /> {t('admin.snapshot.runExport')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

type BackupImportDialogProps = BackupDialogProps & {
  onCompleted: () => void;
};

export function BackupImportDialog({
  open,
  onOpenChange,
  libraryId,
  t,
  onCompleted,
}: BackupImportDialogProps) {
  const [file, setFile] = useState<File | null>(null);
  const [overwrite, setOverwrite] = useState<LibrarySnapshotOverwriteMode>('reject');
  const [importing, setImporting] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const runImport = async () => {
    if (!file) return;
    setImporting(true);
    try {
      const result = await librarySnapshotApi.import(libraryId, file, overwrite);
      if (result.kind === 'completed') {
        let totalRows = 0;
        for (const rowCount of Object.values(result.report.postgresRowsByTable ?? {})) {
          if (typeof rowCount === 'number') totalRows += rowCount;
        }
        toast.success(t('admin.snapshot.importSuccess', { count: totalRows }));
      } else {
        toast.info(t('admin.snapshot.importAccepted'));
        const operation = await librarySnapshotApi.waitForImport(result.operation.operationId);
        if (operation.status !== 'ready') {
          toast.error(t('admin.snapshot.importOperationFailed'));
          return;
        }
        toast.success(t('admin.snapshot.importReady'));
      }
      onCompleted();
      onOpenChange(false);
    } catch (error: unknown) {
      const fallback = error instanceof Error && error.message === 'snapshot_import_timeout'
        ? t('admin.snapshot.importTimeout')
        : errorMessage(error, t('admin.snapshot.importFailed'));
      toast.error(fallback);
    } finally {
      setImporting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('admin.snapshot.importTitle')}</DialogTitle>
          <DialogDescription>{t('admin.snapshot.importDesc')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div>
            <div className="section-label mb-1.5">{t('admin.snapshot.archiveFile')}</div>
            <input
              ref={inputRef}
              type="file"
              accept=".tar.zst,.zst,application/zstd,application/x-zstd"
              onChange={(event) => setFile(event.target.files?.[0] ?? null)}
              className="w-full text-sm file:mr-3 file:rounded-lg file:border file:bg-muted file:px-3 file:py-1.5 file:text-xs file:font-medium hover:file:bg-accent"
            />
            {file && (
              <div className="mt-1.5 text-xs text-muted-foreground font-mono">
                {file.name} · {(file.size / (1024 * 1024)).toFixed(1)} MiB
              </div>
            )}
          </div>
          <div className="space-y-2">
            <div className="section-label">{t('admin.snapshot.overwriteTitle')}</div>
            <label
              htmlFor="backup-overwrite-reject"
              className="flex items-start gap-3 rounded-xl bg-surface-sunken p-3 cursor-pointer hover:bg-accent/30 transition-colors"
            >
              <input
                type="radio"
                id="backup-overwrite-reject"
                name="backup-overwrite"
                checked={overwrite === 'reject'}
                onChange={() => setOverwrite('reject')}
                className="mt-0.5"
              />
              <div className="text-sm font-semibold">{t('admin.snapshot.overwriteReject')}</div>
            </label>
            <label
              htmlFor="backup-overwrite-replace"
              className="flex items-start gap-3 rounded-xl bg-surface-sunken p-3 cursor-pointer hover:bg-accent/30 transition-colors"
            >
              <input
                type="radio"
                id="backup-overwrite-replace"
                name="backup-overwrite"
                checked={overwrite === 'replace'}
                onChange={() => setOverwrite('replace')}
                className="mt-0.5"
              />
              <div className="min-w-0">
                <div className="text-sm font-semibold">{t('admin.snapshot.overwriteReplace')}</div>
                {overwrite === 'replace' && (
                  <div className="mt-1 flex items-start gap-1.5 text-xs text-status-warning">
                    <AlertTriangle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                    {t('admin.snapshot.overwriteReplaceWarn')}
                  </div>
                )}
              </div>
            </label>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={importing}>
            {t('common.cancel')}
          </Button>
          <Button onClick={runImport} disabled={!file || importing}>
            {importing ? (
              <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
            ) : (
              <Upload className="h-3.5 w-3.5 mr-1.5" />
            )}
            {t('admin.snapshot.runImport')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
