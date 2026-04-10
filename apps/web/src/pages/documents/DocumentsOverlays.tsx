import type { RefObject } from 'react';
import type { TFunction } from 'i18next';
import { File, Loader2, RotateCw, Trash2, Upload, XCircle } from 'lucide-react';

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import type { DocumentItem } from '@/types';

import { formatSize } from './mappers';

type DocumentsOverlaysProps = {
  activeTab: 'documents' | 'web';
  addLinkOpen: boolean;
  appendContent: string;
  appendLoading: boolean;
  appendTextOpen: boolean;
  boundaryPolicy: string;
  clearSelection: () => void;
  crawlMode: string;
  deleteDocOpen: boolean;
  handleAppendText: () => void;
  handleBulkCancel: () => void;
  handleBulkDelete: () => void;
  handleBulkReprocess: () => void;
  handleDelete: () => void;
  handleReplaceFile: () => void;
  handleStartWebIngest: () => void;
  maxDepth: string;
  maxPages: string;
  replaceFile: File | null;
  replaceFileInputRef: RefObject<HTMLInputElement | null>;
  replaceFileOpen: boolean;
  replaceLoading: boolean;
  seedUrl: string;
  selectedCount: number;
  selectedDoc: DocumentItem | null;
  setAddLinkOpen: (open: boolean) => void;
  setAppendContent: (value: string) => void;
  setAppendTextOpen: (open: boolean) => void;
  setBoundaryPolicy: (value: string) => void;
  setCrawlMode: (value: string) => void;
  setDeleteDocOpen: (open: boolean) => void;
  setMaxDepth: (value: string) => void;
  setMaxPages: (value: string) => void;
  setReplaceFile: (file: File | null) => void;
  setReplaceFileOpen: (open: boolean) => void;
  setSeedUrl: (value: string) => void;
  t: TFunction;
  webIngestLoading: boolean;
};

export function DocumentsOverlays({
  activeTab,
  addLinkOpen,
  appendContent,
  appendLoading,
  appendTextOpen,
  boundaryPolicy,
  clearSelection,
  crawlMode,
  deleteDocOpen,
  handleAppendText,
  handleBulkCancel,
  handleBulkDelete,
  handleBulkReprocess,
  handleDelete,
  handleReplaceFile,
  handleStartWebIngest,
  maxDepth,
  maxPages,
  replaceFile,
  replaceFileInputRef,
  replaceFileOpen,
  replaceLoading,
  seedUrl,
  selectedCount,
  selectedDoc,
  setAddLinkOpen,
  setAppendContent,
  setAppendTextOpen,
  setBoundaryPolicy,
  setCrawlMode,
  setDeleteDocOpen,
  setMaxDepth,
  setMaxPages,
  setReplaceFile,
  setReplaceFileOpen,
  setSeedUrl,
  t,
  webIngestLoading,
}: DocumentsOverlaysProps) {
  return (
    <>
      {activeTab === 'documents' && selectedCount > 0 && (
        <div className="sticky bottom-0 z-10 flex items-center gap-3 border-t bg-background px-4 py-3 shadow-lg">
          <span className="text-sm font-medium tabular-nums">
            {t('documents.nSelected', { count: selectedCount })}
          </span>
          <Button variant="destructive" size="sm" onClick={handleBulkDelete}>
            <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('documents.deleteSelected')}
          </Button>
          <Button variant="outline" size="sm" onClick={handleBulkCancel}>
            <XCircle className="h-3.5 w-3.5 mr-1.5" /> {t('documents.cancelProcessing')}
          </Button>
          <Button variant="outline" size="sm" onClick={handleBulkReprocess}>
            <RotateCw className="h-3.5 w-3.5 mr-1.5" /> {t('documents.retrySelected')}
          </Button>
          <div className="flex-1" />
          <Button variant="ghost" size="sm" onClick={clearSelection}>
            {t('documents.clearSelection')}
          </Button>
        </div>
      )}

      <Dialog open={addLinkOpen} onOpenChange={setAddLinkOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('documents.addWebContent')}</DialogTitle>
            <DialogDescription>{t('documents.addWebContentDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <Label>{t('documents.seedUrl')}</Label>
              <Input
                value={seedUrl}
                onChange={event => setSeedUrl(event.target.value)}
                placeholder={t('documents.seedUrlPlaceholder')}
                className="mt-2"
              />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <Label>{t('documents.mode')}</Label>
                <Select value={crawlMode} onValueChange={setCrawlMode}>
                  <SelectTrigger className="mt-2"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="single_page">{t('documents.singlePage')}</SelectItem>
                    <SelectItem value="recursive_crawl">{t('documents.recursiveCrawl')}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div>
                <Label>{t('documents.boundary')}</Label>
                <Select value={boundaryPolicy} onValueChange={setBoundaryPolicy}>
                  <SelectTrigger className="mt-2"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="same_host">{t('documents.sameHost')}</SelectItem>
                    <SelectItem value="allow_external">{t('documents.allowExternal')}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
            {crawlMode === 'recursive_crawl' && (
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <Label>{t('documents.maxDepth')}</Label>
                  <Input type="number" value={maxDepth} onChange={event => setMaxDepth(event.target.value)} min="1" max="10" className="mt-2" />
                </div>
                <div>
                  <Label>{t('documents.maxPages')}</Label>
                  <Input type="number" value={maxPages} onChange={event => setMaxPages(event.target.value)} min="1" max="500" className="mt-2" />
                </div>
              </div>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setAddLinkOpen(false)}>{t('documents.cancel')}</Button>
            <Button disabled={!seedUrl.trim() || webIngestLoading} onClick={handleStartWebIngest}>
              {webIngestLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.starting')}</> : t('documents.startIngest')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={deleteDocOpen} onOpenChange={setDeleteDocOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('documents.deleteDoc')}</DialogTitle>
            <DialogDescription>{t('documents.confirmDelete', { name: selectedDoc?.fileName })}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDocOpen(false)}>{t('documents.cancel')}</Button>
            <Button variant="destructive" onClick={handleDelete}>{t('documents.delete')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={appendTextOpen} onOpenChange={open => { setAppendTextOpen(open); if (!open) setAppendContent(''); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('documents.appendTextTitle')}</DialogTitle>
            <DialogDescription>{t('documents.appendTextDesc', { name: selectedDoc?.fileName })}</DialogDescription>
          </DialogHeader>
          <div>
            <Label>{t('documents.textContent')}</Label>
            <textarea
              className="w-full h-32 border rounded-xl p-3.5 text-sm mt-2 resize-none bg-card focus:outline-none focus:ring-2 focus:ring-primary/30 focus:border-primary/40 transition-all"
              placeholder={t('documents.appendTextPlaceholder')}
              value={appendContent}
              onChange={event => setAppendContent(event.target.value)}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => { setAppendTextOpen(false); setAppendContent(''); }}>{t('documents.cancel')}</Button>
            <Button disabled={!appendContent.trim() || appendLoading} onClick={handleAppendText}>
              {appendLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.append')}...</> : t('documents.append')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={replaceFileOpen} onOpenChange={open => { setReplaceFileOpen(open); if (!open) setReplaceFile(null); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('documents.replaceFileTitle')}</DialogTitle>
            <DialogDescription>{t('documents.replaceFileDesc', { name: selectedDoc?.fileName })}</DialogDescription>
          </DialogHeader>
          <div
            className="border-2 border-dashed rounded-xl p-10 text-center transition-all duration-200 hover:border-primary/40 hover:bg-primary/5 cursor-pointer hover:shadow-soft"
            onClick={() => replaceFileInputRef.current?.click()}
            onDragOver={event => event.preventDefault()}
            onDrop={event => {
              event.preventDefault();
              const file = event.dataTransfer.files[0];
              if (file) {
                setReplaceFile(file);
              }
            }}
          >
            <input
              ref={replaceFileInputRef}
              type="file"
              className="hidden"
              onChange={event => {
                const file = event.target.files?.[0];
                if (file) {
                  setReplaceFile(file);
                }
                event.target.value = '';
              }}
            />
            {replaceFile ? (
              <>
                <File className="h-8 w-8 text-primary mx-auto mb-3" />
                <p className="text-sm font-bold">{replaceFile.name}</p>
                <p className="text-xs text-muted-foreground mt-1.5">{formatSize(replaceFile.size)}</p>
              </>
            ) : (
              <>
                <Upload className="h-8 w-8 text-muted-foreground mx-auto mb-3" />
                <p className="text-sm font-bold">{t('documents.selectFile')}</p>
                <p className="text-xs text-muted-foreground mt-1.5">{t('documents.selectFileHint')}</p>
              </>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => { setReplaceFileOpen(false); setReplaceFile(null); }}>{t('documents.cancel')}</Button>
            <Button disabled={!replaceFile || replaceLoading} onClick={handleReplaceFile}>
              {replaceLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.replace')}...</> : t('documents.replace')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
