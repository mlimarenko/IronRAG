import { useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { AlertTriangle, Download, Loader2, Upload } from 'lucide-react';

import { librarySnapshotApi } from '@/api';
import type {
  LibrarySnapshotIncludeKind,
  LibrarySnapshotOverwriteMode,
} from '@/api/documents';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { errorMessage } from '@/lib/errorMessage';

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

  const runExport = () => {
    const kinds: LibrarySnapshotIncludeKind[] = ['library_data'];
    if (includeBlobs) kinds.push('blobs');
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
          <div className="rounded-xl border p-3">
            <div className="text-sm font-semibold">{t('admin.snapshot.libraryDataLabel')}</div>
            <div className="text-xs text-muted-foreground mt-1">
              {t('admin.snapshot.libraryDataDesc')}
            </div>
          </div>
          <label
            htmlFor="backup-include-blobs"
            className="flex items-start gap-3 rounded-xl border p-3 cursor-pointer hover:bg-accent/30 transition-colors"
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
      const report = await librarySnapshotApi.import(libraryId, file, overwrite);
      const totalRows = Object.values(report.postgresRowsByTable ?? {}).reduce(
        (sum, n) => sum + (typeof n === 'number' ? n : 0),
        0,
      );
      toast.success(t('admin.snapshot.importSuccess', { count: totalRows }));
      onCompleted();
      onOpenChange(false);
    } catch (error: unknown) {
      toast.error(errorMessage(error, t('admin.snapshot.importFailed')));
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
              className="w-full text-sm file:mr-3 file:rounded-md file:border file:bg-muted file:px-3 file:py-1.5 file:text-xs file:font-medium hover:file:bg-accent"
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
              className="flex items-start gap-3 rounded-xl border p-3 cursor-pointer hover:bg-accent/30 transition-colors"
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
              className="flex items-start gap-3 rounded-xl border p-3 cursor-pointer hover:bg-accent/30 transition-colors"
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
                    <AlertTriangle className="h-3 w-3 mt-0.5 shrink-0" />
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
