import type { ChangeEvent, RefObject } from 'react';
import type { TFunction } from 'i18next';
import { CheckCircle2, Link as LinkIcon, Loader2, Upload, XCircle } from 'lucide-react';

import { Button } from '@/components/ui/button';

type UploadQueueItem = {
  name: string;
  state: 'uploading' | 'done' | 'error';
  error?: string;
};

type DocumentsPageHeaderProps = {
  activeLibraryName: string;
  activeTab: 'documents' | 'web';
  documentsCount: number;
  fileInputRef: RefObject<HTMLInputElement | null>;
  handleFileSelect: (event: ChangeEvent<HTMLInputElement>) => void;
  setActiveTab: (tab: 'documents' | 'web') => void;
  setAddLinkOpen: (open: boolean) => void;
  setBoundaryPolicy: (value: string) => void;
  setCrawlMode: (value: string) => void;
  setMaxDepth: (value: string) => void;
  setMaxPages: (value: string) => void;
  setSeedUrl: (value: string) => void;
  t: TFunction;
  uploadQueue: UploadQueueItem[];
  webRunsCount: number;
};

export function DocumentsPageHeader({
  activeLibraryName,
  activeTab,
  documentsCount,
  fileInputRef,
  handleFileSelect,
  setActiveTab,
  setAddLinkOpen,
  setBoundaryPolicy,
  setCrawlMode,
  setMaxDepth,
  setMaxPages,
  setSeedUrl,
  t,
  uploadQueue,
  webRunsCount,
}: DocumentsPageHeaderProps) {
  return (
    <div className="page-header">
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-lg font-bold tracking-tight">{t('documents.title')}</h1>
          <p className="text-sm text-muted-foreground">
            {activeLibraryName} - {t('documents.subtitle')}
          </p>
        </div>

        <div className="flex gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
          <button
            className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${
              activeTab === 'documents'
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setActiveTab('documents')}
          >
            {t('documents.tabs.documents')}
            <span className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === 'documents' ? 'bg-primary-foreground/20' : 'bg-background/60'}`}>{documentsCount}</span>
          </button>
          <button
            className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${
              activeTab === 'web'
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setActiveTab('web')}
          >
            {t('documents.tabs.webIngest')}
            <span className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === 'web' ? 'bg-primary-foreground/20' : 'bg-background/60'}`}>{webRunsCount}</span>
          </button>
        </div>

        <div className="flex gap-2">
          {activeTab === 'documents' && (
            <Button size="sm" onClick={() => fileInputRef.current?.click()}>
              <Upload className="h-3.5 w-3.5 mr-1.5" /> {t('documents.upload')}
            </Button>
          )}
          {activeTab === 'web' && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setSeedUrl('');
                setCrawlMode('recursive_crawl');
                setBoundaryPolicy('same_host');
                setMaxDepth('3');
                setMaxPages('30');
                setAddLinkOpen(true);
              }}
            >
              <LinkIcon className="h-3.5 w-3.5 mr-1.5" /> {t('documents.addLink')}
            </Button>
          )}
          <input ref={fileInputRef} type="file" multiple className="hidden" onChange={handleFileSelect} />
        </div>
      </div>

      {uploadQueue.length > 0 && (
        <div className="mt-3 space-y-1.5">
          {uploadQueue.map((upload, index) => (
            <div key={index} className="flex items-center gap-2.5 text-xs p-3 rounded-xl bg-card border shadow-soft">
              {upload.state === 'uploading' ? (
                <Loader2 className="h-3 w-3 animate-spin text-primary" />
              ) : upload.state === 'done' ? (
                <CheckCircle2 className="h-3 w-3 text-status-ready" />
              ) : (
                <XCircle className="h-3 w-3 text-status-failed" />
              )}
              <span className="font-semibold">{upload.name}</span>
              <span className="text-muted-foreground ml-auto">
                {upload.state === 'uploading'
                  ? t('documents.uploading')
                  : upload.state === 'done'
                    ? t('documents.queued')
                    : upload.error}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
