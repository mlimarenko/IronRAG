import { AlertTriangle, Download, LoaderCircle, Upload } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { Button, Modal } from '@/shared/ui'
import { errorMessage } from '@/shared/lib/errorMessage'
import {
  downloadLibrarySnapshot,
  downloadWorkspaceSnapshot,
  type RestoreSnapshotInput,
} from '@/features/admin/libraries/api/useLibrariesAdmin'
import type { AdminLibraryRow } from '@/features/admin/libraries/model/librariesAdapter'

type SnapshotDialogProps = {
  row: AdminLibraryRow
  open: boolean
  onOpenChange: (open: boolean) => void
}

function ToggleRow({
  checked,
  disabled = false,
  label,
  description,
  onChange,
}: Readonly<{
  checked: boolean
  disabled?: boolean
  label: string
  description: string
  onChange: (checked: boolean) => void
}>) {
  return (
    <label className="snapshot-option">
      <input
        type="checkbox"
        aria-label={label}
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span>
        <strong>{label}</strong>
        <small>{description}</small>
      </span>
    </label>
  )
}

export function SnapshotExportDialog({ row, open, onOpenChange }: Readonly<SnapshotDialogProps>) {
  const { t } = useTranslation()
  const [includeBlobs, setIncludeBlobs] = useState(true)
  const [includeWorkspace, setIncludeWorkspace] = useState(false)
  const [includeAiConfig, setIncludeAiConfig] = useState(false)

  const includedKinds = () => {
    const include: Array<'library_data' | 'blobs' | 'workspace' | 'ai_config'> = ['library_data']
    if (includeBlobs) include.push('blobs')
    if (includeWorkspace) include.push('workspace')
    if (includeAiConfig) include.push('ai_config')
    return include
  }

  const exportLibrary = () => {
    downloadLibrarySnapshot(row.id, includedKinds())
    toast.success(t('admin.snapshot.exportSuccess'))
    onOpenChange(false)
  }

  const exportWorkspace = () => {
    downloadWorkspaceSnapshot(row.workspace.id, includedKinds())
    toast.success(t('admin.libraries.snapshot.workspaceExportSuccess'))
    onOpenChange(false)
  }

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={t('admin.snapshot.exportTitle')}
      description={t('admin.snapshot.exportDesc')}
      footer={
        <>
          <Button onClick={() => onOpenChange(false)}>{t('common.cancel')}</Button>
          <Button onClick={exportWorkspace}>
            <Download size={14} />
            {t('admin.libraries.snapshot.exportWorkspace')}
          </Button>
          <Button variant="primary" onClick={exportLibrary}>
            <Download size={14} />
            {t('admin.libraries.snapshot.exportLibrary')}
          </Button>
        </>
      }
    >
      <div className="snapshot-options">
        <ToggleRow
          checked
          disabled
          label={t('admin.snapshot.libraryDataLabel')}
          description={t('admin.snapshot.libraryDataDesc')}
          onChange={() => undefined}
        />
        <ToggleRow
          checked={includeBlobs}
          label={t('admin.snapshot.includeBlobsLabel')}
          description={t('admin.snapshot.includeBlobsDesc')}
          onChange={setIncludeBlobs}
        />
        <ToggleRow
          checked={includeWorkspace}
          label={t('admin.snapshot.includeWorkspaceLabel')}
          description={t('admin.snapshot.includeWorkspaceDesc')}
          onChange={setIncludeWorkspace}
        />
        <ToggleRow
          checked={includeAiConfig}
          label={t('admin.snapshot.includeAiConfigLabel')}
          description={t('admin.snapshot.includeAiConfigDesc')}
          onChange={setIncludeAiConfig}
        />
      </div>
    </Modal>
  )
}

type SnapshotRestoreDialogProps = SnapshotDialogProps & {
  busy: boolean
  onRestore: (input: RestoreSnapshotInput) => Promise<void>
}

export function SnapshotRestoreDialog({
  row,
  open,
  onOpenChange,
  busy,
  onRestore,
}: Readonly<SnapshotRestoreDialogProps>) {
  const { t } = useTranslation()
  const [file, setFile] = useState<File | null>(null)
  const [scope, setScope] = useState<'library' | 'workspace'>('library')
  const [overwrite, setOverwrite] = useState<'reject' | 'replace'>('reject')

  const runRestore = async () => {
    if (!file) return
    try {
      await onRestore({
        scope,
        libraryId: row.id,
        workspaceId: row.workspace.id,
        file,
        overwrite,
      })
      toast.success(
        scope === 'library'
          ? t('admin.snapshot.importReady')
          : t('admin.libraries.snapshot.workspaceImportReady'),
      )
      onOpenChange(false)
    } catch (error: unknown) {
      toast.error(errorMessage(error, t('admin.snapshot.importFailed')))
    }
  }

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={t('admin.snapshot.importTitle')}
      description={t('admin.snapshot.importDesc', { productName: getBranding().displayName })}
      footer={
        <>
          <Button disabled={busy} onClick={() => onOpenChange(false)}>
            {t('common.cancel')}
          </Button>
          <Button variant="primary" disabled={!file || busy} onClick={runRestore}>
            {busy ? <LoaderCircle className="spin" size={14} /> : <Upload size={14} />}
            {t('admin.snapshot.runImport')}
          </Button>
        </>
      }
    >
      <label className="file-drop">
        <Upload size={24} />
        <strong>{file?.name ?? t('admin.libraries.snapshot.chooseArchive')}</strong>
        <span>
          {file
            ? t('admin.libraries.snapshot.archiveSize', {
                size: (file.size / (1024 * 1024)).toFixed(1),
              })
            : t('admin.libraries.snapshot.archiveHint')}
        </span>
        <input
          type="file"
          accept=".tar.zst,.zst,.tar,application/zstd,application/x-tar"
          onChange={(event) => setFile(event.target.files?.[0] ?? null)}
        />
      </label>
      <div className="snapshot-restore-grid">
        <label className="field">
          <span>{t('admin.libraries.snapshot.restoreScope')}</span>
          <select
            className="input"
            value={scope}
            onChange={(event) => setScope(event.target.value as 'library' | 'workspace')}
          >
            <option value="library">{t('admin.libraries.snapshot.scopeLibrary')}</option>
            <option value="workspace">{t('admin.libraries.snapshot.scopeWorkspace')}</option>
          </select>
        </label>
        <label className="field">
          <span>{t('admin.snapshot.overwriteTitle')}</span>
          <select
            className="input"
            value={overwrite}
            onChange={(event) => setOverwrite(event.target.value as 'reject' | 'replace')}
          >
            <option value="reject">{t('admin.snapshot.overwriteReject')}</option>
            <option value="replace">{t('admin.snapshot.overwriteReplace')}</option>
          </select>
        </label>
      </div>
      {overwrite === 'replace' && (
        <div className="snapshot-warning">
          <AlertTriangle size={15} />
          <span>{t('admin.snapshot.overwriteReplaceWarn')}</span>
        </div>
      )}
    </Modal>
  )
}
import { getBranding } from '@/app/branding'
