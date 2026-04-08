import { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useSearchParams } from 'react-router-dom';
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
  CheckCircle2, Clock, X, File, ArrowUpDown, Globe, ExternalLink,
  CheckSquare
} from 'lucide-react';
import { humanizeDocumentFailure, humanizeDocumentStage } from '@/lib/document-processing';
import type { DocumentItem, DocumentReadiness, DocumentStatus } from '@/types';

type DocumentsStatusFilter = 'all' | 'in_progress' | 'ready' | 'failed';
const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;

function parseStatusFilter(value: string | null): DocumentsStatusFilter {
  if (value === 'in_progress' || value === 'ready' || value === 'failed') {
    return value;
  }

  return 'all';
}

function parseReadinessFilter(value: string | null): DocumentReadiness | null {
  if (
    value === 'processing' ||
    value === 'readable' ||
    value === 'graph_sparse' ||
    value === 'graph_ready' ||
    value === 'failed'
  ) {
    return value;
  }

  return null;
}

function parsePageSize(value: string | null): (typeof PAGE_SIZE_OPTIONS)[number] {
  const parsed = Number.parseInt(value ?? '', 10);

  if (PAGE_SIZE_OPTIONS.includes(parsed as (typeof PAGE_SIZE_OPTIONS)[number])) {
    return parsed as (typeof PAGE_SIZE_OPTIONS)[number];
  }

  return PAGE_SIZE_OPTIONS[0];
}

function parsePage(value: string | null): number {
  const parsed = Number.parseInt(value ?? '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 1;
}

/** Map a single API response item to the UI's DocumentItem shape. */
function mapApiDocument(raw: any, t: ReturnType<typeof useTranslation>['t']): DocumentItem {
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
    failureMessage = humanizeDocumentFailure({
      failureCode,
      stalledReason: raw.readinessSummary?.stalledReason ?? raw.readiness_summary?.stalled_reason,
      stage: jobStage,
    }, t);
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
    stage: humanizeDocumentStage(jobStage, t),
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

export default function DocumentsPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const [searchParams, setSearchParams] = useSearchParams();
  const [documents, setDocuments] = useState<DocumentItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedDoc, setSelectedDoc] = useState<DocumentItem | null>(null);
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
  const [crawlMode, setCrawlMode] = useState('recursive_crawl');
  const [boundaryPolicy, setBoundaryPolicy] = useState('same_host');
  const [maxDepth, setMaxDepth] = useState('3');
  const [maxPages, setMaxPages] = useState('100');
  const [webIngestLoading, setWebIngestLoading] = useState(false);

  // Tab state
  const [activeTab, setActiveTab] = useState<'documents' | 'web'>('documents');

  // Bulk selection
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [selectionMode, setSelectionMode] = useState(false);

  // Web ingest runs
  const [webRuns, setWebRuns] = useState<any[]>([]);
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runPages, setRunPages] = useState<any[]>([]);

  const searchQuery = searchParams.get('q') ?? '';
  const statusFilter = parseStatusFilter(searchParams.get('status'));
  const readinessFilter = parseReadinessFilter(searchParams.get('readiness'));
  const selectedDocumentId = searchParams.get('documentId');
  const pageSize = parsePageSize(searchParams.get('pageSize'));
  const requestedPage = parsePage(searchParams.get('page'));
  const readinessConfig: Record<DocumentReadiness, { label: string; cls: string }> = {
    processing: { label: t('dashboard.readinessLabels.processing'), cls: 'status-processing' },
    readable: { label: t('dashboard.readinessLabels.readable'), cls: 'status-warning' },
    graph_sparse: { label: t('dashboard.readinessLabels.graph_sparse'), cls: 'status-warning' },
    graph_ready: { label: t('dashboard.readinessLabels.graph_ready'), cls: 'status-ready' },
    failed: { label: t('dashboard.readinessLabels.failed'), cls: 'status-failed' },
  };

  const updateSearchParamState = useCallback((updates: Record<string, string | null>) => {
    const next = new URLSearchParams(searchParams);

    for (const [key, value] of Object.entries(updates)) {
      if (value == null || value === '') {
        next.delete(key);
      } else {
        next.set(key, value);
      }
    }

    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams]);

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
        const doc = mapApiDocument(r, t);
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
  }, [activeLibrary, t]);

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
      updateSearchParamState({ documentId: null });
      await fetchDocuments();
    } catch (err: any) {
      console.error('Delete failed:', err);
      toast.error(err?.message || "Failed to delete document");
    }
  }, [selectedDoc, fetchDocuments, updateSearchParamState]);

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.reprocess(selectedDoc.id);
      await fetchDocuments();
      // Refresh the selected doc
      const raw = await documentsApi.get(selectedDoc.id);
      setSelectedDoc(mapApiDocument(raw, t));
    } catch (err: any) {
      console.error('Reprocess failed:', err);
      toast.error(err?.message || "Failed to reprocess document");
    }
  }, [selectedDoc, fetchDocuments, t]);

  const handleSelectDoc = useCallback(async (doc: DocumentItem, syncQuery = true) => {
    if (syncQuery) {
      updateSearchParamState({ documentId: doc.id });
    }
    setSelectedDoc(doc);
    setInspectorSegments(null);
    setInspectorFacts(null);
    try {
      const raw = await documentsApi.get(doc.id);
      setSelectedDoc(mapApiDocument(raw, t));
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
  }, [t, updateSearchParamState]);

  useEffect(() => {
    if (!selectedDocumentId) {
      setSelectedDoc(null);
      setInspectorSegments(null);
      setInspectorFacts(null);
      return;
    }

    if (selectedDoc?.id === selectedDocumentId) {
      return;
    }

    const matched = documents.find(doc => doc.id === selectedDocumentId);
    if (matched) {
      void handleSelectDoc(matched, false);
      return;
    }

    setSelectedDoc(null);
    setInspectorSegments(null);
    setInspectorFacts(null);
  }, [documents, handleSelectDoc, selectedDoc?.id, selectedDocumentId]);

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
      setCrawlMode('recursive_crawl');
      setBoundaryPolicy('same_host');
      setMaxDepth('3');
      setMaxPages('30');
      await fetchDocuments();
    } catch (err: any) {
      toast.error(err?.message || t('documents.webIngestFailed'));
    } finally {
      setWebIngestLoading(false);
    }
  }, [activeLibrary, seedUrl, crawlMode, boundaryPolicy, maxDepth, maxPages, fetchDocuments, t]);

  // --- Bulk selection helpers ---
  const toggleSelection = (id: string) => {
    setSelectedIds(prev => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const clearSelection = () => {
    setSelectedIds(new Set());
    setSelectionMode(false);
  };

  const selectedCount = selectedIds.size;

  // Escape exits selection mode
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && selectionMode) clearSelection();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [selectionMode]);

  const handleBulkDelete = async () => {
    if (!confirm(t('documents.confirmBulkDelete', { count: selectedCount }))) return;
    try {
      await documentsApi.batchDelete(Array.from(selectedIds));
      toast.success(t('documents.bulkDeleteSuccess', { count: selectedCount }));
      clearSelection();
      await fetchDocuments();
    } catch {
      toast.error(t('documents.bulkDeleteFailed'));
    }
  };

  const handleBulkCancel = async () => {
    try {
      await documentsApi.batchCancel(Array.from(selectedIds));
      toast.success(t('documents.bulkCancelSuccess', { count: selectedCount }));
      clearSelection();
      await fetchDocuments();
    } catch {
      toast.error(t('documents.bulkCancelFailed'));
    }
  };

  const handleBulkReprocess = async () => {
    try {
      await documentsApi.batchReprocess(Array.from(selectedIds));
      toast.success(t('documents.bulkReprocessSuccess', { count: selectedCount }));
      clearSelection();
      await fetchDocuments();
    } catch {
      toast.error(t('documents.bulkReprocessFailed'));
    }
  };

  const filteredDocuments = documents.filter(d => {
    if (searchQuery && !d.fileName.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    if (readinessFilter && d.readiness !== readinessFilter) return false;
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
  const selectedDocPage = selectedDocumentId
    ? (() => {
        const index = filteredDocuments.findIndex(doc => doc.id === selectedDocumentId);
        return index >= 0 ? Math.floor(index / pageSize) + 1 : null;
      })()
    : null;
  const totalPages = Math.max(1, Math.ceil(filteredDocuments.length / pageSize));
  const currentPage = selectedDocPage ?? Math.min(requestedPage, totalPages);
  const pageStart = (currentPage - 1) * pageSize;
  const pagedDocuments = filteredDocuments.slice(pageStart, pageStart + pageSize);
  const visibleRangeStart = filteredDocuments.length > 0 ? pageStart + 1 : 0;
  const visibleRangeEnd = filteredDocuments.length > 0 ? pageStart + pagedDocuments.length : 0;

  useEffect(() => {
    if (selectedDocPage == null || selectedDocPage === requestedPage) {
      return;
    }

    updateSearchParamState({
      page: selectedDocPage > 1 ? String(selectedDocPage) : null,
    });
  }, [requestedPage, selectedDocPage, updateSearchParamState]);

  useEffect(() => {
    if (selectedDocPage != null || requestedPage <= totalPages) {
      return;
    }

    updateSearchParamState({
      page: totalPages > 1 ? String(totalPages) : null,
    });
  }, [requestedPage, selectedDocPage, totalPages, updateSearchParamState]);

  const statCounts = {
    total: documents.length,
    ready: documents.filter(d => d.readiness === 'graph_ready' || d.readiness === 'readable' || d.readiness === 'graph_sparse').length,
    readable: documents.filter(d => d.readiness === 'readable').length,
    graphSparse: documents.filter(d => d.readiness === 'graph_sparse').length,
    processing: documents.filter(d => d.readiness === 'processing').length,
    failed: documents.filter(d => d.readiness === 'failed').length,
  };

  const toggleSort = (field: string) => {
    if (sortField === field) setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    else { setSortField(field); setSortDir('desc'); }
  };

  const readinessFilterLabel = readinessFilter
    ? t(`dashboard.readinessLabels.${readinessFilter}`)
    : null;
  const hasActiveFilters = Boolean(searchQuery || statusFilter !== 'all' || readinessFilter);
  const showReadinessChip = Boolean(
    readinessFilter && readinessFilter !== 'graph_sparse' && readinessFilter !== 'readable',
  );

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

          {/* Tab switcher */}
          <div className="flex gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
            <button
              className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${activeTab === 'documents' ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:text-foreground'}`}
              onClick={() => setActiveTab('documents')}
            >
              {t('documents.tabs.documents')}
              <span className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === 'documents' ? 'bg-primary-foreground/20' : 'bg-background/60'}`}>{documents.length}</span>
            </button>
            <button
              className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${activeTab === 'web' ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:text-foreground'}`}
              onClick={() => setActiveTab('web')}
            >
              {t('documents.tabs.webIngest')}
              <span className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === 'web' ? 'bg-primary-foreground/20' : 'bg-background/60'}`}>{webRuns.length}</span>
            </button>
          </div>

          <div className="flex gap-2">
            {activeTab === 'documents' && (
              <Button size="sm" onClick={() => fileInputRef.current?.click()}>
                <Upload className="h-3.5 w-3.5 mr-1.5" /> {t('documents.upload')}
              </Button>
            )}
            {activeTab === 'web' && (
              <Button size="sm" variant="outline" onClick={() => {
                setSeedUrl('');
                setCrawlMode('recursive_crawl');
                setBoundaryPolicy('same_host');
                setMaxDepth('3');
                setMaxPages('30');
                setAddLinkOpen(true);
              }}>
                <LinkIcon className="h-3.5 w-3.5 mr-1.5" /> {t('documents.addLink')}
              </Button>
            )}
            <input ref={fileInputRef} type="file" multiple className="hidden" onChange={handleFileSelect} />
          </div>
        </div>


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

      {/* Filters — documents tab only */}
      {activeTab === 'documents' && <div className="px-6 py-3 border-b flex flex-wrap items-center gap-3 bg-surface-sunken/50">
        <div className="relative flex-1 min-w-[200px] max-w-sm">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input
            className="h-9 pl-9 text-sm"
            placeholder={t('documents.searchPlaceholder')}
            value={searchQuery}
            onChange={e => updateSearchParamState({
              q: e.target.value || null,
              documentId: null,
              page: null,
            })}
          />
        </div>
        <div className="flex gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
          {[
            {
              key: 'all',
              label: t('documents.all'),
              count: statCounts.total,
              icon: null,
              active: statusFilter === 'all' && !readinessFilter,
              updates: { status: null, readiness: null, documentId: null, page: null },
            },
            {
              key: 'in_progress',
              label: t('documents.inProgress'),
              count: statCounts.processing,
              icon: <Clock className="h-3 w-3 text-status-processing" />,
              active: statusFilter === 'in_progress',
              updates: { status: 'in_progress', readiness: null, documentId: null, page: null },
            },
            {
              key: 'ready',
              label: t('documents.ready'),
              count: statCounts.ready,
              icon: <CheckCircle2 className="h-3 w-3 text-status-ready" />,
              active: statusFilter === 'ready',
              updates: { status: 'ready', readiness: null, documentId: null, page: null },
            },
            {
              key: 'readable',
              label: t('dashboard.readableNoGraph'),
              count: statCounts.readable,
              icon: <AlertTriangle className="h-3 w-3 text-status-warning" />,
              active: readinessFilter === 'readable',
              updates: { status: null, readiness: 'readable', documentId: null, page: null },
            },
            {
              key: 'graph_sparse',
              label: t('documents.sparseTab'),
              count: statCounts.graphSparse,
              icon: <AlertTriangle className="h-3 w-3 text-status-sparse" />,
              active: readinessFilter === 'graph_sparse',
              updates: { status: null, readiness: 'graph_sparse', documentId: null, page: null },
            },
            {
              key: 'failed',
              label: t('documents.failedTab'),
              count: statCounts.failed,
              icon: <XCircle className="h-3 w-3 text-status-failed" />,
              active: statusFilter === 'failed',
              updates: { status: 'failed', readiness: null, documentId: null, page: null },
            },
          ].map(filter => (
            <button
              key={filter.key}
              className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${filter.active ? 'bg-card shadow-soft font-semibold text-foreground' : 'text-muted-foreground hover:text-foreground'}`}
              onClick={() => updateSearchParamState(filter.updates)}
            >
              {filter.icon}
              {filter.label}
              {filter.count > 0 && <span className="tabular-nums text-[10px] opacity-70">{filter.count}</span>}
            </button>
          ))}
        </div>
        {showReadinessChip && readinessFilter && readinessFilterLabel && (
          <button
            className={`h-8 inline-flex items-center gap-2 px-3 text-xs rounded-full font-semibold ${readinessConfig[readinessFilter].cls}`}
            onClick={() => updateSearchParamState({ readiness: null, documentId: null, page: null })}
          >
            <span>{readinessFilterLabel}</span>
            <X className="h-3 w-3" />
          </button>
        )}
        <span className="text-xs text-muted-foreground font-semibold tabular-nums">{filteredDocuments.length} {t('documents.of')} {documents.length}</span>
        {(() => {
          const totalCost = documents.reduce((sum, d) => sum + (d.cost ?? 0), 0);
          return totalCost > 0 ? (
            <span className="text-xs text-muted-foreground ml-auto mr-2">{t('documents.totalCost')}: <span className="font-bold tabular-nums">${totalCost.toFixed(3)}</span></span>
          ) : null;
        })()}
        <Button
          size="sm"
          variant={selectionMode ? 'default' : 'outline'}
          className={`${documents.reduce((s, d) => s + (d.cost ?? 0), 0) > 0 ? '' : 'ml-auto'} h-8 text-xs`}
          onClick={() => selectionMode ? clearSelection() : setSelectionMode(true)}
        >
          <CheckSquare className="h-3.5 w-3.5 mr-1.5" />
          {selectionMode ? t('documents.cancelSelection') : t('documents.select')}
        </Button>
      </div>}

      {/* Main area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Content area */}
        <div
          className={`flex-1 min-w-0 overflow-hidden ${activeTab === 'documents' && dragOver ? 'ring-2 ring-primary ring-inset bg-primary/5' : ''}`}
          onDragOver={activeTab === 'documents' ? (e => { e.preventDefault(); setDragOver(true); }) : undefined}
          onDragLeave={activeTab === 'documents' ? (() => setDragOver(false)) : undefined}
          onDrop={activeTab === 'documents' ? handleDrop : undefined}
        >
          {activeTab === 'documents' ? (
          <>
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
          ) : filteredDocuments.length === 0 ? (
            <div className="empty-state py-20">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <FileText className="h-7 w-7 text-muted-foreground" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{hasActiveFilters ? t('documents.noMatchingDocs') : t('documents.noDocs')}</h2>
              <p className="text-sm text-muted-foreground mt-2">
                {hasActiveFilters ? t('documents.noMatchingDocsDesc') : t('documents.noDocsDesc')}
              </p>
            </div>
          ) : (
            <div className="flex h-full min-h-0 flex-col">
              <div className="min-h-0 flex-1 overflow-auto">
                <table className="w-full text-sm">
                  <thead className="sticky top-0 z-10" style={{
                    background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))',
                    backdropFilter: 'blur(8px)',
                  }}>
                    <tr className="border-b text-left">
                      {selectionMode && (
                        <th className="px-4 py-3 w-10">
                          <input
                            type="checkbox"
                            checked={pagedDocuments.length > 0 && pagedDocuments.every(d => selectedIds.has(d.id))}
                            onChange={() => {
                              const pageFullySelected =
                                pagedDocuments.length > 0 && pagedDocuments.every(d => selectedIds.has(d.id));

                              setSelectedIds(prev => {
                                const next = new Set(prev);

                                for (const doc of pagedDocuments) {
                                  if (pageFullySelected) {
                                    next.delete(doc.id);
                                  } else {
                                    next.add(doc.id);
                                  }
                                }

                                return next;
                              });
                            }}
                            className="h-4 w-4 rounded border-gray-300"
                          />
                        </th>
                      )}
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
                    {pagedDocuments.map(doc => {
                      const rc = readinessConfig[doc.readiness];
                      return (
                        <tr
                          key={doc.id}
                          className={`border-b cursor-pointer transition-all duration-150 ${selectedIds.has(doc.id) ? 'bg-primary/10' : selectedDoc?.id === doc.id ? 'bg-primary/5 border-l-2 border-l-primary' : 'hover:bg-accent/30'}`}
                          onClick={() => selectionMode ? toggleSelection(doc.id) : handleSelectDoc(doc)}
                        >
                          {selectionMode && (
                            <td className="px-4 py-3.5 w-10">
                              <input
                                type="checkbox"
                                checked={selectedIds.has(doc.id)}
                                onChange={(e) => {
                                  e.stopPropagation();
                                  toggleSelection(doc.id);
                                }}
                                onClick={(e) => e.stopPropagation()}
                                className="h-4 w-4 rounded border-gray-300"
                              />
                            </td>
                          )}
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
              </div>

              <div className="shrink-0 border-t bg-background/95 px-4 py-3 shadow-[0_-8px_24px_hsl(var(--background)/0.9)] backdrop-blur supports-[backdrop-filter]:bg-background/85">
                <div className="flex flex-wrap items-center gap-3">
                  <span className="text-xs font-medium text-muted-foreground tabular-nums">
                    {t('documents.paginationSummary', {
                      from: visibleRangeStart,
                      to: visibleRangeEnd,
                      total: filteredDocuments.length,
                    })}
                  </span>

                  <div className="flex items-center gap-2 md:ml-auto">
                    <span className="text-xs text-muted-foreground">{t('documents.pageSize')}</span>
                    <Select
                      value={String(pageSize)}
                      onValueChange={value => updateSearchParamState({
                        pageSize: value === String(PAGE_SIZE_OPTIONS[0]) ? null : value,
                        page: null,
                      })}
                    >
                      <SelectTrigger className="h-8 w-[92px] text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {PAGE_SIZE_OPTIONS.map(option => (
                          <SelectItem key={option} value={String(option)}>
                            {option}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>

                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-8 text-xs"
                      disabled={currentPage <= 1}
                      onClick={() => updateSearchParamState({
                        page: currentPage - 1 > 1 ? String(currentPage - 1) : null,
                        documentId: null,
                      })}
                    >
                      {t('documents.previous')}
                    </Button>

                    <span className="min-w-[112px] text-center text-xs font-medium text-muted-foreground tabular-nums">
                      {t('documents.pageLabel', { page: currentPage, total: totalPages })}
                    </span>

                    <Button
                      variant="outline"
                      size="sm"
                      className="h-8 text-xs"
                      disabled={currentPage >= totalPages}
                      onClick={() => updateSearchParamState({
                        page: String(currentPage + 1),
                        documentId: null,
                      })}
                    >
                      {t('documents.next')}
                    </Button>
                  </div>
                </div>
              </div>
            </div>
          )}

          </>
          ) : (
          <>
          {/* Web Ingest Runs — web tab */}
          {(() => {
            const terminalStates = new Set(['completed', 'completed_partial', 'failed']);
            const activeRuns = webRuns.filter((r: any) => !terminalStates.has(r.runState?.toLowerCase()));
            return activeRuns.length > 0 ? (
              <div className="mx-4 mt-4 flex items-center gap-2 text-xs px-3 py-2 rounded-xl bg-card border shadow-soft">
                <Loader2 className="h-3 w-3 animate-spin text-primary" />
                <span className="font-semibold">{activeRuns.length} web ingest {activeRuns.length === 1 ? 'run' : 'runs'} in progress</span>
              </div>
            ) : null;
          })()}
          {webRuns.length > 0 ? (
            <div className="m-4 border rounded-xl">
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
                        setMaxDepth(String(run.maxDepth ?? 3));
                        setMaxPages(String(run.maxPages ?? 100));
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
          ) : (
            <div className="empty-state py-20">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <Globe className="h-7 w-7 text-muted-foreground" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{t('documents.webIngestRuns')}</h2>
              <p className="text-sm text-muted-foreground mt-2">{t('documents.noDocsDesc')}</p>
            </div>
          )}
          </>
          )}
        </div>

        {/* Inspector panel */}
        {selectedDoc && (
          <div className={`inspector-panel w-80 lg:w-96 shrink-0 hidden md:block overflow-y-auto animate-slide-in-right ${selectionMode ? 'opacity-40 pointer-events-none' : ''}`}>
            <div className="p-4 border-b flex items-center justify-between">
              <h3 className="text-sm font-bold truncate tracking-tight">{selectedDoc.fileName}</h3>
              <button onClick={() => updateSearchParamState({ documentId: null })} className="p-1.5 rounded-lg hover:bg-muted transition-colors" aria-label="Close inspector">
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
                    <XCircle className="h-3.5 w-3.5" /> {t('documents.error')}
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

      {/* Bulk action toolbar — documents tab only */}
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
            {crawlMode === 'recursive_crawl' && <div className="grid grid-cols-2 gap-3">
              <div><Label>{t('documents.maxDepth')}</Label><Input type="number" value={maxDepth} onChange={e => setMaxDepth(e.target.value)} min="1" max="10" className="mt-2" /></div>
              <div><Label>{t('documents.maxPages')}</Label><Input type="number" value={maxPages} onChange={e => setMaxPages(e.target.value)} min="1" max="500" className="mt-2" /></div>
            </div>}
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
