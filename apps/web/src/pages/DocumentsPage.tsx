import { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { documentsApi, billingApi, apiFetch } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter,
} from '@/components/ui/dialog';
import {
  Upload, Link as LinkIcon, Search, FileText, Loader2, XCircle,
  RotateCw, Trash2, Download, Plus, AlertTriangle,
  CheckCircle2, Clock, X, File, ArrowUpDown, Globe, ExternalLink
} from 'lucide-react';
import type { DocumentItem, DocumentReadiness, DocumentStatus } from '@/types';

/** Map a single API response item to the UI's DocumentItem shape. */
function mapApiDocument(raw: any): DocumentItem {
  const fileName: string = raw.fileName ?? raw.document?.external_key ?? 'unknown';
  const ext = fileName.includes('.') ? fileName.split('.').pop()!.toLowerCase() : '';
  const mimeType: string = raw.activeRevision?.mime_type ?? raw.active_revision?.mime_type ?? '';
  const fileType = ext || mimeType.split('/').pop() || 'file';
  const fileSize: number = raw.activeRevision?.byte_size ?? raw.active_revision?.byte_size ?? 0;
  const uploadedAt: string = raw.document?.created_at ?? '';

  // Derive readiness from readinessSummary or pipeline state
  const readinessKind: string = raw.readinessSummary?.readinessKind ?? raw.readiness_summary?.readiness_kind ?? '';
  const jobState: string = raw.pipeline?.latest_job?.queue_state ?? '';
  const jobStage: string | undefined = raw.pipeline?.latest_job?.current_stage ?? undefined;
  const failureCode: string | undefined = raw.pipeline?.latest_job?.failure_code ?? undefined;
  const retryable: boolean = raw.pipeline?.latest_job?.retryable ?? false;
  const activityStatus: string = raw.readinessSummary?.activityStatus ?? raw.readiness_summary?.activity_status ?? '';

  let readiness: DocumentReadiness = 'processing';
  if (readinessKind === 'graph_ready') readiness = 'graph_ready';
  else if (readinessKind === 'graph_sparse') readiness = 'graph_sparse';
  else if (readinessKind === 'readable') readiness = 'readable';
  else if (readinessKind === 'failed' || jobState === 'failed') readiness = 'failed';
  else readiness = 'processing';

  let status: DocumentStatus = 'processing';
  if (readiness === 'graph_ready') status = 'ready';
  else if (readiness === 'readable') status = 'ready';
  else if (readiness === 'graph_sparse') status = 'ready_no_graph';
  else if (readiness === 'failed') status = 'failed';
  else if (jobState === 'queued' || activityStatus === 'queued') status = 'queued';
  else status = 'processing';

  let failureMessage: string | undefined;
  if (readiness === 'failed') {
    failureMessage = failureCode ?? raw.readinessSummary?.stalledReason ?? raw.readiness_summary?.stalled_reason ?? 'Processing failed';
  }

  const rev = raw.activeRevision ?? raw.active_revision;
  const sourceKind: string | undefined = rev?.content_source_kind ?? undefined;
  const sourceUri: string | undefined = rev?.source_uri ?? undefined;

  return {
    id: raw.document?.id ?? raw.id ?? '',
    fileName,
    fileType,
    fileSize,
    uploadedAt,
    cost: null,
    status,
    readiness,
    stage: jobStage,
    failureMessage,
    canRetry: readiness === 'failed' ? retryable : undefined,
    sourceKind,
    sourceUri,
  };
}

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

const readinessConfig: Record<DocumentReadiness, { label: string; cls: string }> = {
  processing: { label: 'Processing', cls: 'status-processing' },
  readable: { label: 'Readable', cls: 'status-processing' },
  graph_sparse: { label: 'Graph Sparse', cls: 'status-warning' },
  graph_ready: { label: 'Graph Ready', cls: 'status-ready' },
  failed: { label: 'Failed', cls: 'status-failed' },
};

export default function DocumentsPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const [documents, setDocuments] = useState<DocumentItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedDoc, setSelectedDoc] = useState<DocumentItem | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [statusFilter, setStatusFilter] = useState('all');
  const [sortField, setSortField] = useState<string>('uploadedAt');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('desc');

  const [addLinkOpen, setAddLinkOpen] = useState(false);
  const [deleteDocOpen, setDeleteDocOpen] = useState(false);
  const [appendTextOpen, setAppendTextOpen] = useState(false);
  const [replaceFileOpen, setReplaceFileOpen] = useState(false);

  const [dragOver, setDragOver] = useState(false);
  const [uploadQueue, setUploadQueue] = useState<{ name: string; state: 'uploading' | 'done' | 'error'; error?: string }[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [appendContent, setAppendContent] = useState('');
  const [appendLoading, setAppendLoading] = useState(false);
  const [replaceFile, setReplaceFile] = useState<File | null>(null);
  const [replaceLoading, setReplaceLoading] = useState(false);
  const replaceFileInputRef = useRef<HTMLInputElement>(null);
  const [inspectorSegments, setInspectorSegments] = useState<number | null>(null);
  const [inspectorFacts, setInspectorFacts] = useState<number | null>(null);

  const [seedUrl, setSeedUrl] = useState('');
  const [crawlMode, setCrawlMode] = useState('single_page');
  const [boundaryPolicy, setBoundaryPolicy] = useState('same_host');
  const [maxDepth, setMaxDepth] = useState('3');
  const [maxPages, setMaxPages] = useState('100');
  const [webIngestLoading, setWebIngestLoading] = useState(false);

  // Web ingest runs
  const [webRuns, setWebRuns] = useState<any[]>([]);
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runPages, setRunPages] = useState<any[]>([]);

  const fetchDocuments = useCallback(async () => {
    if (!activeLibrary) return;
    setLoading(true);
    setLoadError(null);
    try {
      const [raw, costs] = await Promise.all([
        documentsApi.list(activeLibrary.id),
        billingApi.getLibraryDocumentCosts(activeLibrary.id).catch(() => []),
      ]);
      const costMap = new Map<string, number>();
      for (const c of costs) {
        costMap.set(c.documentId, parseFloat(c.totalCost));
      }
      const items = (Array.isArray(raw) ? raw : []).map((r: any) => {
        const doc = mapApiDocument(r);
        const cost = costMap.get(doc.id);
        if (cost != null && !isNaN(cost)) {
          doc.cost = cost;
        }
        return doc;
      });
      setDocuments(items);
      // Also load web ingest runs
      try {
        const runsRaw = await apiFetch<any>(`/content/web-runs?libraryId=${activeLibrary.id}`);
        const runs = Array.isArray(runsRaw) ? runsRaw : runsRaw?.items ?? [];
        setWebRuns(runs);
      } catch { setWebRuns([]); }
    } catch (err: any) {
      setLoadError(err?.message ?? 'Failed to load documents');
      setDocuments([]);
    } finally {
      setLoading(false);
    }
  }, [activeLibrary]);

  useEffect(() => {
    fetchDocuments();
  }, [fetchDocuments]);

  const uploadFiles = useCallback(async (files: File[]) => {
    if (!activeLibrary) return;
    const items = files.map(f => ({ name: f.name, state: 'uploading' as const }));
    setUploadQueue(prev => [...prev, ...items]);
    for (const file of files) {
      try {
        await documentsApi.upload(activeLibrary.id, file);
        setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'done' } : u));
      } catch (err: any) {
        setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'error', error: err?.message ?? 'Upload failed' } : u));
      }
    }
    // Refresh list after all uploads complete
    await fetchDocuments();
    setTimeout(() => setUploadQueue([]), 3000);
  }, [activeLibrary, fetchDocuments]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const files = Array.from(e.dataTransfer.files);
    uploadFiles(files);
  }, [uploadFiles]);

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    uploadFiles(files);
    e.target.value = '';
  };

  const handleDelete = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.delete(selectedDoc.id);
      setDeleteDocOpen(false);
      setSelectedDoc(null);
      await fetchDocuments();
    } catch (err: any) {
      console.error('Delete failed:', err);
      toast.error(err?.message || "Failed to delete document");
    }
  }, [selectedDoc, fetchDocuments]);

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.reprocess(selectedDoc.id);
      await fetchDocuments();
      // Refresh the selected doc
      const raw = await documentsApi.get(selectedDoc.id);
      setSelectedDoc(mapApiDocument(raw));
    } catch (err: any) {
      console.error('Reprocess failed:', err);
      toast.error(err?.message || "Failed to reprocess document");
    }
  }, [selectedDoc, fetchDocuments]);

  const handleSelectDoc = useCallback(async (doc: DocumentItem) => {
    setSelectedDoc(doc);
    setInspectorSegments(null);
    setInspectorFacts(null);
    try {
      const raw = await documentsApi.get(doc.id);
      setSelectedDoc(mapApiDocument(raw));
    } catch {
      // Keep the list-level data if detail fetch fails
    }
    // Fetch segments and facts counts in parallel
    Promise.all([
      documentsApi.getPreparedSegments(doc.id).catch(() => []),
      documentsApi.getTechnicalFacts(doc.id).catch(() => []),
    ]).then(([segments, facts]) => {
      setInspectorSegments(Array.isArray(segments) ? segments.length : 0);
      setInspectorFacts(Array.isArray(facts) ? facts.length : 0);
    });
  }, []);

  const handleDownloadText = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      const segments = await documentsApi.getPreparedSegments(selectedDoc.id);
      const textParts = (Array.isArray(segments) ? segments : []).map((s: any) => s.text ?? s.content ?? '');
      const blob = new Blob([textParts.join('\n\n')], { type: 'text/plain' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${selectedDoc.fileName.replace(/\.[^.]+$/, '')}.txt`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      toast.success("Text downloaded");
    } catch (err: any) {
      toast.error(err?.message ?? "Failed to download text");
    }
  }, [selectedDoc]);

  const handleAppendText = useCallback(async () => {
    if (!selectedDoc || !appendContent.trim()) return;
    setAppendLoading(true);
    try {
      await documentsApi.append(selectedDoc.id, appendContent);
      toast.success("Text appended successfully");
      setAppendTextOpen(false);
      setAppendContent('');
      await fetchDocuments();
    } catch (err: any) {
      toast.error(err?.message ?? "Failed to append text");
    } finally {
      setAppendLoading(false);
    }
  }, [selectedDoc, appendContent, fetchDocuments]);

  const handleReplaceFile = useCallback(async () => {
    if (!selectedDoc || !replaceFile) return;
    setReplaceLoading(true);
    try {
      await documentsApi.replace(selectedDoc.id, replaceFile);
      toast.success("File replaced successfully");
      setReplaceFileOpen(false);
      setReplaceFile(null);
      await fetchDocuments();
    } catch (err: any) {
      toast.error(err?.message ?? "Failed to replace file");
    } finally {
      setReplaceLoading(false);
    }
  }, [selectedDoc, replaceFile, fetchDocuments]);

  const handleStartWebIngest = useCallback(async () => {
    if (!activeLibrary || !seedUrl.trim()) return;
    let url = seedUrl.trim();
    // Auto-prefix https:// if no protocol specified
    if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
    // Basic URL validation
    try { new URL(url); } catch { toast.error(t('documents.invalidUrl')); return; }
    setWebIngestLoading(true);
    try {
      await documentsApi.createWebIngestRun({
        libraryId: activeLibrary.id,
        seedUrl: url,
        mode: crawlMode,
        boundaryPolicy,
        maxDepth: parseInt(maxDepth, 10),
        maxPages: parseInt(maxPages, 10),
      });
      toast.success(t('documents.webIngestStarted'));
      setAddLinkOpen(false);
      setSeedUrl('');
      setCrawlMode('single_page');
      setBoundaryPolicy('same_host');
      setMaxDepth('3');
      setMaxPages('100');
      await fetchDocuments();
    } catch (err: any) {
      toast.error(err?.message || t('documents.webIngestFailed'));
    } finally {
      setWebIngestLoading(false);
    }
  }, [activeLibrary, seedUrl, crawlMode, boundaryPolicy, maxDepth, maxPages, fetchDocuments, t]);

  const filtered = documents.filter(d => {
    if (searchQuery && !d.fileName.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    if (statusFilter === 'in_progress') return d.readiness === 'processing';
    if (statusFilter === 'ready') return d.readiness === 'graph_ready' || d.readiness === 'readable' || d.readiness === 'graph_sparse';
    if (statusFilter === 'failed') return d.readiness === 'failed';
    return true;
  }).sort((a, b) => {
    const dir = sortDir === 'asc' ? 1 : -1;
    if (sortField === 'fileName') return a.fileName.localeCompare(b.fileName) * dir;
    if (sortField === 'fileSize') return (a.fileSize - b.fileSize) * dir;
    if (sortField === 'cost') return ((a.cost ?? 0) - (b.cost ?? 0)) * dir;
    return (new Date(a.uploadedAt).getTime() - new Date(b.uploadedAt).getTime()) * dir;
  });

  const statCounts = {
    total: documents.length,
    graphReady: documents.filter(d => d.readiness === 'graph_ready').length,
    graphSparse: documents.filter(d => d.readiness === 'graph_sparse').length,
    processing: documents.filter(d => d.readiness === 'processing').length,
    failed: documents.filter(d => d.readiness === 'failed').length,
  };

  const toggleSort = (field: string) => {
    if (sortField === field) setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    else { setSortField(field); setSortDir('desc'); }
  };

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header"><h1 className="text-lg font-bold tracking-tight">{t('documents.title')}</h1></div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <FileText className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('documents.noLibrary')}</h2>
          <p className="text-sm text-muted-foreground mt-2">{t('documents.noLibraryDesc')}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Header */}
      <div className="page-header">
        <div className="flex items-center justify-between gap-4 flex-wrap">
          <div>
            <h1 className="text-lg font-bold tracking-tight">{t('documents.title')}</h1>
            <p className="text-sm text-muted-foreground">{activeLibrary.name} — {t('documents.subtitle')}</p>
          </div>
          <div className="flex gap-2">
            <Button size="sm" onClick={() => fileInputRef.current?.click()}>
              <Upload className="h-3.5 w-3.5 mr-1.5" /> {t('documents.upload')}
            </Button>
            <Button size="sm" variant="outline" onClick={() => setAddLinkOpen(true)}>
              <LinkIcon className="h-3.5 w-3.5 mr-1.5" /> {t('documents.addLink')}
            </Button>
            <input ref={fileInputRef} type="file" multiple className="hidden" onChange={handleFileSelect} />
          </div>
        </div>

        {/* Stats */}
        <div className="flex flex-wrap gap-4 mt-3 text-xs">
          <span className="text-muted-foreground font-semibold">{statCounts.total} {t('documents.total')}</span>
          <span className="flex items-center gap-1.5"><CheckCircle2 className="h-3 w-3 text-status-ready" /><span className="font-semibold">{statCounts.graphReady}</span> {t('documents.graphReady')}</span>
          <span className="flex items-center gap-1.5"><AlertTriangle className="h-3 w-3 text-status-sparse" /><span className="font-semibold">{statCounts.graphSparse}</span> {t('documents.sparse')}</span>
          <span className="flex items-center gap-1.5"><Clock className="h-3 w-3 text-status-processing" /><span className="font-semibold">{statCounts.processing}</span> {t('documents.processing')}</span>
          <span className="flex items-center gap-1.5"><XCircle className="h-3 w-3 text-status-failed" /><span className="font-semibold">{statCounts.failed}</span> {t('documents.failed')}</span>
          {(() => {
            const totalCost = documents.reduce((sum, d) => sum + (d.cost ?? 0), 0);
            return totalCost > 0 ? (
              <span className="flex items-center gap-1.5 ml-auto"><span className="text-muted-foreground">{t('documents.totalCost')}:</span> <span className="font-bold tabular-nums">${totalCost.toFixed(3)}</span></span>
            ) : null;
          })()}
        </div>

        {/* Web ingest activity strip — will be wired when web-runs API integration is added */}

        {/* Upload queue */}
        {uploadQueue.length > 0 && (
          <div className="mt-3 space-y-1.5">
            {uploadQueue.map((u, i) => (
              <div key={i} className="flex items-center gap-2.5 text-xs p-3 rounded-xl bg-card border shadow-soft">
                {u.state === 'uploading' ? <Loader2 className="h-3 w-3 animate-spin text-primary" /> : u.state === 'done' ? <CheckCircle2 className="h-3 w-3 text-status-ready" /> : <XCircle className="h-3 w-3 text-status-failed" />}
                <span className="font-semibold">{u.name}</span>
                <span className="text-muted-foreground ml-auto">{u.state === 'uploading' ? t('documents.uploading') : u.state === 'done' ? t('documents.queued') : u.error}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Filters */}
      <div className="px-6 py-3 border-b flex flex-wrap items-center gap-3 bg-surface-sunken/50">
        <div className="relative flex-1 min-w-[200px] max-w-sm">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input className="h-9 pl-9 text-sm" placeholder={t('documents.searchPlaceholder')} value={searchQuery} onChange={e => setSearchQuery(e.target.value)} />
        </div>
        <div className="flex gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
          {['all', 'in_progress', 'ready', 'failed'].map(f => (
            <button
              key={f}
              className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium ${statusFilter === f ? 'bg-card shadow-soft font-semibold text-foreground' : 'text-muted-foreground hover:text-foreground'}`}
              onClick={() => setStatusFilter(f)}
            >
              {f === 'all' ? t('documents.all') : f === 'in_progress' ? t('documents.inProgress') : f === 'ready' ? t('documents.ready') : t('documents.failedTab')}
            </button>
          ))}
        </div>
        <span className="text-xs text-muted-foreground font-semibold tabular-nums">{filtered.length} {t('documents.of')} {documents.length}</span>
      </div>

      {/* Main area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Drop zone + table */}
        <div
          className={`flex-1 overflow-auto ${dragOver ? 'ring-2 ring-primary ring-inset bg-primary/5' : ''}`}
          onDragOver={e => { e.preventDefault(); setDragOver(true); }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
        >
          {dragOver && (
            <div className="absolute inset-0 z-10 flex items-center justify-center pointer-events-none">
              <div className="p-8 rounded-2xl border-2 border-dashed border-primary bg-card shadow-elevated">
                <Upload className="h-8 w-8 text-primary mx-auto mb-3" />
                <p className="text-sm font-bold">{t('documents.dropToUpload')}</p>
              </div>
            </div>
          )}

          {loading ? (
            <div className="empty-state py-20">
              <Loader2 className="h-7 w-7 animate-spin text-primary mb-4" />
              <h2 className="text-base font-bold tracking-tight">{t('documents.loadingDocs')}</h2>
            </div>
          ) : loadError ? (
            <div className="empty-state py-20">
              <div className="w-14 h-14 rounded-2xl bg-destructive/10 flex items-center justify-center mb-4">
                <XCircle className="h-7 w-7 text-destructive" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{t('documents.failedToLoad')}</h2>
              <p className="text-sm text-muted-foreground mt-2">{loadError}</p>
              <Button size="sm" variant="outline" className="mt-4" onClick={fetchDocuments}>
                <RotateCw className="h-3.5 w-3.5 mr-1.5" /> {t('documents.retry')}
              </Button>
            </div>
          ) : filtered.length === 0 ? (
            <div className="empty-state py-20">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <FileText className="h-7 w-7 text-muted-foreground" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{searchQuery || statusFilter !== 'all' ? t('documents.noMatchingDocs') : t('documents.noDocs')}</h2>
              <p className="text-sm text-muted-foreground mt-2">
                {searchQuery || statusFilter !== 'all' ? t('documents.noMatchingDocsDesc') : t('documents.noDocsDesc')}
              </p>
            </div>
          ) : (
            <table className="w-full text-sm">
              <thead className="sticky top-0 z-10" style={{
                background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))',
                backdropFilter: 'blur(8px)',
              }}>
                <tr className="border-b text-left">
                  {[
                    { key: 'fileName', label: t('documents.name') },
                    { key: 'fileType', label: t('documents.type') },
                    { key: 'fileSize', label: t('documents.size') },
                    { key: 'uploadedAt', label: t('documents.uploaded') },
                    { key: 'cost', label: t('documents.cost') },
                    { key: 'status', label: t('documents.status') },
                  ].map(col => (
                    <th key={col.key} className="px-4 py-3 section-label">
                      <button className="flex items-center gap-1 hover:text-foreground transition-colors" onClick={() => toggleSort(col.key)}>
                        {col.label}
                        {sortField === col.key && <ArrowUpDown className="h-3 w-3" />}
                      </button>
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {filtered.map(doc => {
                  const rc = readinessConfig[doc.readiness];
                  return (
                    <tr
                      key={doc.id}
                      className={`border-b cursor-pointer transition-all duration-150 ${selectedDoc?.id === doc.id ? 'bg-primary/5 border-l-2 border-l-primary' : 'hover:bg-accent/30'}`}
                      onClick={() => handleSelectDoc(doc)}
                    >
                      <td className="px-4 py-3.5">
                        <div className="flex items-center gap-3">
                          <div className={`w-8 h-8 rounded-xl flex items-center justify-center shrink-0 ${doc.sourceKind === 'web_page' ? 'bg-blue-100 dark:bg-blue-900/30' : 'bg-surface-sunken'}`}>
                            {doc.sourceKind === 'web_page' ? <Globe className="h-3.5 w-3.5 text-blue-600 dark:text-blue-400" /> : <File className="h-3.5 w-3.5 text-muted-foreground" />}
                          </div>
                          <div className="min-w-0">
                            <span className="truncate block max-w-[200px] font-semibold">{doc.fileName}</span>
                            {doc.sourceKind === 'web_page' && doc.sourceUri && (
                              <span className="truncate block max-w-[200px] text-[10px] text-muted-foreground">{doc.sourceUri}</span>
                            )}
                          </div>
                        </div>
                      </td>
                      <td className="px-4 py-3.5 text-muted-foreground uppercase text-[10px] font-bold tracking-widest">{doc.fileType}</td>
                      <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">{formatSize(doc.fileSize)}</td>
                      <td className="px-4 py-3.5 text-muted-foreground text-xs">{formatDate(doc.uploadedAt)}</td>
                      <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">{doc.cost != null ? `$${doc.cost.toFixed(3)}` : '—'}</td>
                      <td className="px-4 py-3.5">
                        <div className="flex items-center gap-2">
                          <span className={`status-badge ${rc.cls}`}>{rc.label}</span>
                          {doc.progressPercent != null && (
                            <span className="text-xs text-muted-foreground tabular-nums font-medium">{doc.progressPercent}%</span>
                          )}
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}

          {/* Web Ingest Runs */}
          {webRuns.length > 0 && (
            <div className="mt-4 border rounded-xl">
              <div className="px-4 py-3 border-b flex items-center gap-2">
                <Globe className="h-4 w-4 text-muted-foreground" />
                <span className="text-sm font-semibold">{t('documents.webIngestRuns')}</span>
                <span className="text-xs text-muted-foreground ml-auto">{webRuns.length}</span>
              </div>
              <div className="divide-y">
                {webRuns.slice(0, 10).map((run: any) => (
                  <div key={run.runId}>
                    <button
                      className="w-full px-4 py-2.5 flex items-center gap-3 text-left hover:bg-accent/30 transition-colors text-xs"
                      onClick={async () => {
                        if (expandedRunId === run.runId) { setExpandedRunId(null); setRunPages([]); return; }
                        setExpandedRunId(run.runId);
                        try {
                          const pages = await apiFetch<any>(`/content/web-runs/${run.runId}/pages`);
                          setRunPages(Array.isArray(pages) ? pages : pages?.items ?? []);
                        } catch { setRunPages([]); }
                      }}
                    >
                      <span className={`status-badge ${run.runState === 'completed' ? 'status-ready' : run.runState === 'failed' ? 'status-failed' : 'status-processing'}`}>
                        {run.runState}
                      </span>
                      <span className="truncate font-medium">{run.seedUrl}</span>
                      <span className="text-muted-foreground shrink-0">
                        {run.mode === 'single_page' ? t('documents.singlePage') : run.mode === 'recursive_crawl' ? t('documents.recursiveCrawl') : run.mode}
                      </span>
                      {run.mode === 'recursive_crawl' && (
                        <span className="text-muted-foreground shrink-0">{t('documents.maxDepth')}: {run.maxDepth} · {t('documents.maxPages')}: {run.maxPages}</span>
                      )}
                      <span className="text-muted-foreground shrink-0">{run.counts?.processed ?? 0}/{run.counts?.discovered ?? 0} {t('documents.pages')}</span>
                      <Button variant="ghost" size="icon" className="h-6 w-6 shrink-0 ml-auto" onClick={(e) => {
                        e.stopPropagation();
                        setSeedUrl(run.seedUrl);
                        setCrawlMode(run.mode);
                        setBoundaryPolicy(run.boundaryPolicy || 'same_host');
                        setMaxDepth(String(run.maxDepth ?? 1));
                        setMaxPages(String(run.maxPages ?? 10));
                        setAddLinkOpen(true);
                      }}>
                        <RotateCw className="h-3 w-3" />
                      </Button>
                    </button>
                    {expandedRunId === run.runId && runPages.length > 0 && (
                      <div className="bg-muted/30 px-4 py-2 space-y-1">
                        {runPages.map((page: any, i: number) => (
                          <div key={i} className="flex items-center gap-2 text-[11px]">
                            <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                              page.candidateState === 'processed' ? 'bg-green-500' :
                              page.candidateState === 'failed' ? 'bg-red-500' :
                              page.candidateState === 'excluded' ? 'bg-yellow-500' : 'bg-gray-400'
                            }`} />
                            <span className="truncate text-muted-foreground">{page.normalizedUrl ?? page.discoveredUrl ?? '?'}</span>
                            <span className="text-[10px] text-muted-foreground shrink-0">{page.candidateState}</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Inspector panel */}
        {selectedDoc && (
          <div className="inspector-panel w-80 lg:w-96 shrink-0 hidden md:block overflow-y-auto animate-slide-in-right">
            <div className="p-4 border-b flex items-center justify-between">
              <h3 className="text-sm font-bold truncate tracking-tight">{selectedDoc.fileName}</h3>
              <button onClick={() => setSelectedDoc(null)} className="p-1.5 rounded-lg hover:bg-muted transition-colors" aria-label="Close inspector">
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-4 space-y-5">
              <div>
                <span className={`status-badge ${readinessConfig[selectedDoc.readiness].cls}`}>
                  {readinessConfig[selectedDoc.readiness].label}
                </span>
                {selectedDoc.stage && <span className="text-xs text-muted-foreground ml-2">{selectedDoc.stage}</span>}
              </div>

              {selectedDoc.failureMessage && (
                <div className="inline-error">
                  <div className="flex items-center gap-1.5 font-bold text-destructive mb-1.5">
                    <XCircle className="h-3.5 w-3.5" /> Error
                  </div>
                  {selectedDoc.failureMessage}
                </div>
              )}

              {selectedDoc.progressPercent != null && (
                <div>
                  <div className="flex justify-between text-xs mb-2">
                    <span className="font-semibold">Progress</span>
                    <span className="tabular-nums font-medium">{selectedDoc.progressPercent}%</span>
                  </div>
                  <div className="h-2 bg-surface-sunken rounded-full overflow-hidden" style={{ boxShadow: 'inset 0 1px 2px hsl(var(--foreground) / 0.04)' }}>
                    <div className="h-full bg-primary rounded-full transition-all duration-500" style={{
                      width: `${selectedDoc.progressPercent}%`,
                      boxShadow: '0 0 8px -2px hsl(var(--primary) / 0.4)',
                    }} />
                  </div>
                </div>
              )}

              {/* Source info — different for web vs upload */}
              {selectedDoc.sourceKind === 'web_page' && selectedDoc.sourceUri ? (
                <div className="space-y-2.5">
                  <div className="section-label flex items-center gap-1.5">
                    <Globe className="h-3 w-3" /> {t('documents.webSource')}
                  </div>
                  <a href={selectedDoc.sourceUri} target="_blank" rel="noopener noreferrer"
                    className="text-xs text-primary hover:underline flex items-center gap-1 truncate">
                    {selectedDoc.sourceUri} <ExternalLink className="h-3 w-3 shrink-0" />
                  </a>
                  {[
                    [t('documents.type'), selectedDoc.fileType.toUpperCase()],
                    [t('documents.size'), formatSize(selectedDoc.fileSize)],
                    [t('documents.uploaded'), formatDate(selectedDoc.uploadedAt)],
                    [t('documents.cost'), selectedDoc.cost != null ? `$${selectedDoc.cost.toFixed(3)}` : '—'],
                  ].map(([k, v]) => (
                    <div key={k} className="flex justify-between text-sm">
                      <span className="text-muted-foreground">{k}</span>
                      <span className="font-mono text-xs font-semibold">{v}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="space-y-2.5">
                  <div className="section-label">{t('documents.fileInfo')}</div>
                  {[
                    [t('documents.type'), selectedDoc.fileType.toUpperCase()],
                    [t('documents.size'), formatSize(selectedDoc.fileSize)],
                    [t('documents.uploaded'), formatDate(selectedDoc.uploadedAt)],
                    [t('documents.cost'), selectedDoc.cost != null ? `$${selectedDoc.cost.toFixed(3)}` : '—'],
                    ['Document ID', selectedDoc.id],
                  ].map(([k, v]) => (
                    <div key={k} className="flex justify-between text-sm">
                      <span className="text-muted-foreground">{k}</span>
                      <span className="font-mono text-xs font-semibold">{v}</span>
                    </div>
                  ))}
                </div>
              )}

              <div className="space-y-2.5">
                <div className="section-label">{t('documents.preparation')}</div>
                <div className="text-xs text-muted-foreground">
                  {selectedDoc.readiness === 'graph_ready' || selectedDoc.readiness === 'readable' || selectedDoc.readiness === 'graph_sparse' ? (
                    <div className="space-y-1.5">
                      <div className="flex justify-between"><span>Segments</span><span className="font-semibold text-foreground">{inspectorSegments ?? '...'}</span></div>
                      <div className="flex justify-between"><span>Technical Facts</span><span className="font-semibold text-foreground">{inspectorFacts ?? '...'}</span></div>
                      <div className="flex justify-between"><span>Source Format</span><span className="font-semibold text-foreground">{selectedDoc.fileType.toUpperCase()}</span></div>
                    </div>
                  ) : selectedDoc.readiness === 'processing' ? (
                    <div className="flex items-center gap-2"><Loader2 className="h-3 w-3 animate-spin text-primary" /> Processing...</div>
                  ) : (
                    <span>Not yet available</span>
                  )}
                </div>
              </div>

              <div className="space-y-1.5">
                <div className="section-label">{t('documents.actions')}</div>
                {selectedDoc.canRetry && (
                  <Button variant="outline" size="sm" className="w-full justify-start" onClick={handleRetry}>
                    <RotateCw className="h-3.5 w-3.5 mr-2" /> {t('documents.retryProcessing')}
                  </Button>
                )}
                {selectedDoc.sourceKind === 'web_page' && selectedDoc.sourceUri && (
                  <Button variant="outline" size="sm" className="w-full justify-start" onClick={() => {
                    setSeedUrl(selectedDoc.sourceUri || '');
                    setCrawlMode('single_page');
                    setMaxDepth('1');
                    setMaxPages('10');
                    setAddLinkOpen(true);
                  }}>
                    <Globe className="h-3.5 w-3.5 mr-2" /> {t('documents.reIngest')}
                  </Button>
                )}
                <Button variant="outline" size="sm" className="w-full justify-start" onClick={() => setAppendTextOpen(true)}>
                  <Plus className="h-3.5 w-3.5 mr-2" /> {t('documents.appendText')}
                </Button>
                <Button variant="outline" size="sm" className="w-full justify-start" onClick={() => setReplaceFileOpen(true)}>
                  <Upload className="h-3.5 w-3.5 mr-2" /> {t('documents.replaceFile')}
                </Button>
                <Button variant="outline" size="sm" className="w-full justify-start" onClick={handleDownloadText}>
                  <Download className="h-3.5 w-3.5 mr-2" /> {t('documents.downloadText')}
                </Button>
                <Button variant="outline" size="sm" className="w-full justify-start text-destructive hover:text-destructive" onClick={() => setDeleteDocOpen(true)}>
                  <Trash2 className="h-3.5 w-3.5 mr-2" /> {t('documents.delete')}
                </Button>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Dialogs */}
      <Dialog open={addLinkOpen} onOpenChange={setAddLinkOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('documents.addWebContent')}</DialogTitle>
            <DialogDescription>{t('documents.addWebContentDesc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div><Label>{t('documents.seedUrl')}</Label><Input value={seedUrl} onChange={e => setSeedUrl(e.target.value)} placeholder="https://docs.example.com" className="mt-2" /></div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>{t('documents.mode')}</Label><Select value={crawlMode} onValueChange={setCrawlMode}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="single_page">{t('documents.singlePage')}</SelectItem><SelectItem value="recursive_crawl">{t('documents.recursiveCrawl')}</SelectItem></SelectContent></Select></div>
              <div><Label>{t('documents.boundary')}</Label><Select value={boundaryPolicy} onValueChange={setBoundaryPolicy}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="same_host">{t('documents.sameHost')}</SelectItem><SelectItem value="allow_external">{t('documents.allowExternal')}</SelectItem></SelectContent></Select></div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>{t('documents.maxDepth')}</Label><Input type="number" value={maxDepth} onChange={e => setMaxDepth(e.target.value)} min="1" max="10" className="mt-2" /></div>
              <div><Label>{t('documents.maxPages')}</Label><Input type="number" value={maxPages} onChange={e => setMaxPages(e.target.value)} min="1" max="500" className="mt-2" /></div>
            </div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setAddLinkOpen(false)}>{t('documents.cancel')}</Button><Button disabled={!seedUrl.trim() || webIngestLoading} onClick={handleStartWebIngest}>{webIngestLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.starting')}</> : t('documents.startIngest')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={deleteDocOpen} onOpenChange={setDeleteDocOpen}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t('documents.deleteDoc')}</DialogTitle><DialogDescription dangerouslySetInnerHTML={{ __html: t('documents.confirmDelete', { name: selectedDoc?.fileName }) }} /></DialogHeader>
          <DialogFooter><Button variant="outline" onClick={() => setDeleteDocOpen(false)}>{t('documents.cancel')}</Button><Button variant="destructive" onClick={handleDelete}>{t('documents.delete')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={appendTextOpen} onOpenChange={v => { setAppendTextOpen(v); if (!v) setAppendContent(''); }}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t('documents.appendTextTitle')}</DialogTitle><DialogDescription>{t('documents.appendTextDesc', { name: selectedDoc?.fileName })}</DialogDescription></DialogHeader>
          <div><Label>{t('documents.textContent')}</Label><textarea className="w-full h-32 border rounded-xl p-3.5 text-sm mt-2 resize-none bg-card focus:outline-none focus:ring-2 focus:ring-primary/30 focus:border-primary/40 transition-all" placeholder={t('documents.appendTextPlaceholder')} value={appendContent} onChange={e => setAppendContent(e.target.value)} /></div>
          <DialogFooter><Button variant="outline" onClick={() => { setAppendTextOpen(false); setAppendContent(''); }}>{t('documents.cancel')}</Button><Button disabled={!appendContent.trim() || appendLoading} onClick={handleAppendText}>{appendLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.append')}...</> : t('documents.append')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={replaceFileOpen} onOpenChange={v => { setReplaceFileOpen(v); if (!v) setReplaceFile(null); }}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t('documents.replaceFileTitle')}</DialogTitle><DialogDescription>{t('documents.replaceFileDesc', { name: selectedDoc?.fileName })}</DialogDescription></DialogHeader>
          <div
            className="border-2 border-dashed rounded-xl p-10 text-center transition-all duration-200 hover:border-primary/40 hover:bg-primary/5 cursor-pointer hover:shadow-soft"
            onClick={() => replaceFileInputRef.current?.click()}
            onDragOver={e => e.preventDefault()}
            onDrop={e => { e.preventDefault(); const f = e.dataTransfer.files[0]; if (f) setReplaceFile(f); }}
          >
            <input ref={replaceFileInputRef} type="file" className="hidden" onChange={e => { const f = e.target.files?.[0]; if (f) setReplaceFile(f); e.target.value = ''; }} />
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
          <DialogFooter><Button variant="outline" onClick={() => { setReplaceFileOpen(false); setReplaceFile(null); }}>{t('documents.cancel')}</Button><Button disabled={!replaceFile || replaceLoading} onClick={handleReplaceFile}>{replaceLoading ? <><Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" /> {t('documents.replace')}...</> : t('documents.replace')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
