import { ExternalLink, FileUp, FolderUp, RotateCw, UploadCloud } from 'lucide-react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'

import { formatBytes } from '@/shared/format'
import { Button, Modal } from '@/shared/ui'
import { DOCUMENT_FILE_INPUT_ACCEPT } from './model/uploadAccept'
import type { UploadQueueController } from './components/documents-page/useUploadQueueController'

export function UploadDialog({ controller }: Readonly<{ controller: UploadQueueController }>) {
  const { t } = useTranslation()
  const {
    cancelUploadDialog,
    confirmUploadDialog,
    duplicateConflict,
    fileInputRef,
    folderInputRef,
    handleFileSelect,
    handleFolderSelect,
    resolveDuplicate,
    setUploadDialogHint,
    uploadDialogFileCount,
    uploadDialogHint,
    uploadDialogOpen,
  } = controller
  useEffect(() => {
    const input = folderInputRef.current
    input?.setAttribute('webkitdirectory', '')
    input?.setAttribute('directory', '')
  }, [folderInputRef])

  return (
    <>
      <input
        ref={fileInputRef}
        className="visually-hidden"
        type="file"
        multiple
        accept={DOCUMENT_FILE_INPUT_ACCEPT}
        onChange={handleFileSelect}
      />
      <input
        ref={folderInputRef}
        className="visually-hidden"
        type="file"
        multiple
        onChange={handleFolderSelect}
      />
      <Modal
        open={uploadDialogOpen}
        onOpenChange={(open) => {
          if (!open) cancelUploadDialog()
        }}
        title={t('documents.uploadDialogTitle')}
        description={t('documents.uploadDialogChosenFiles', { count: uploadDialogFileCount })}
        footer={
          <>
            <Button onClick={cancelUploadDialog}>{t('common.cancel')}</Button>
            <Button variant="primary" onClick={() => void confirmUploadDialog()}>
              <UploadCloud size={15} />
              {t('documents.uploadDialogConfirm')}
            </Button>
          </>
        }
      >
        <div className="upload-choice max-md:grid-cols-1!">
          <button type="button" onClick={() => fileInputRef.current?.click()}>
            <FileUp size={22} />
            <strong>{t('documents.addContentFiles')}</strong>
            <span>{t('documents.selectFileHint')}</span>
          </button>
          <button type="button" onClick={() => folderInputRef.current?.click()}>
            <FolderUp size={22} />
            <strong>{t('documents.addContentFolder')}</strong>
            <span>{t('documents.uploadDialogHintAppliesToAll')}</span>
          </button>
        </div>
        <label className="field upload-hint">
          <span>{t('documents.documentHint')}</span>
          <textarea
            className="input"
            maxLength={1024}
            value={uploadDialogHint}
            onChange={(event) => setUploadDialogHint(event.target.value)}
            placeholder={t('documents.documentHintEditPlaceholder')}
          />
          <small>{t('documents.documentHintHelp')}</small>
        </label>
      </Modal>
      <Modal
        open={Boolean(duplicateConflict)}
        onOpenChange={(open) => {
          if (!open) void resolveDuplicate('skip')
        }}
        title={t('documents.duplicateTitle')}
        description={t('documents.duplicateDescription', {
          name: duplicateConflict?.candidate.name ?? '',
        })}
        footer={
          <>
            <Button variant="primary" onClick={() => void resolveDuplicate('replace')}>
              <RotateCw size={14} />
              {t('documents.duplicateReplace')}
            </Button>
            <Button onClick={() => void resolveDuplicate('open')}>
              <ExternalLink size={14} />
              {t('documents.duplicateOpenExisting')}
            </Button>
            <Button variant="ghost" onClick={() => void resolveDuplicate('skip')}>
              {t('documents.duplicateSkip')}
            </Button>
          </>
        }
      >
        <div className="duplicate-card">
          <p>{formatBytes(duplicateConflict?.candidate.file.size ?? 0)}</p>
        </div>
      </Modal>
    </>
  )
}
