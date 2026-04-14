import { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useSearchParams } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { documentsApi, billingApi, apiFetch } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import {
  Upload, Search, FileText, Loader2, XCircle,
  RotateCw, AlertTriangle,
  CheckCircle2, Clock, X, File, ArrowUpDown, Globe,
  CheckSquare
} from 'lucide-react';
import type { DocumentItem, DocumentLifecycle, DocumentReadiness } from '@/types';
import type {
  RawWebIngestRunListItem,
  RawWebIngestRunPage,
  RawListEnvelope,
} from '@/types/api-responses';
import {
  PAGE_SIZE_OPTIONS,
  buildDocumentStatusBadgeConfig,
  documentStatusBucket,
  documentStatusSortRank,
  formatDate,
  formatDocumentTypeLabel,
  formatSize,
  getDocumentProcessingDurationMs,
  mapApiDocument,
  parsePage,
  parsePageSize,
  parseReadinessFilter,
  parseStatusFilter,
} from '@/pages/documents/mappers';
import type { RawDocumentForUI } from '@/pages/documents/mappers';
import { DocumentsPageHeader } from '@/pages/documents/DocumentsPageHeader';
import { DocumentsInspectorPanel } from '@/pages/documents/DocumentsInspectorPanel';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { DocumentsOverlays } from '@/pages/documents/DocumentsOverlays';
import { DocumentEditorShell } from '@/pages/documents/editor/DocumentEditorShell';
import { isEditorEditableSourceFormat } from '@/pages/documents/editor/editorSurfaceMode';
import { useDocumentEditor } from '@/pages/documents/editor/useDocumentEditor';
import { compactText } from '@/lib/compactText';

export default function DocumentsPage() {
  const { t } = useTranslation();
  const { activeLibrary, locale } = useApp();
  const [searchParams, setSearchParams] = useSearchParams();
  const [documents, setDocuments] = useState<DocumentItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedDoc, setSelectedDoc] = useState<DocumentItem | null>(null);
  const selectedDocRef = useRef<DocumentItem | null>(null);
  useEffect(() => { selectedDocRef.current = selectedDoc; }, [selectedDoc]);
  const [sortField, setSortField] = useState<string>('uploadedAt');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('desc');

  const [addLinkOpen, setAddLinkOpen] = useState(false);
  const [deleteDocOpen, setDeleteDocOpen] = useState(false);
  const [replaceFileOpen, setReplaceFileOpen] = useState(false);

  const [dragOver, setDragOver] = useState(false);
  const [uploadQueue, setUploadQueue] = useState<{ name: string; state: 'uploading' | 'done' | 'error'; error?: string }[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [duplicateConflict, setDuplicateConflict] = useState<{
    file: File;
    existingDocId: string;
    remaining: File[];
  } | null>(null);

  const [replaceFile, setReplaceFile] = useState<File | null>(null);
  const [replaceLoading, setReplaceLoading] = useState(false);
  const replaceFileInputRef = useRef<HTMLInputElement>(null);
  const [inspectorSegments, setInspectorSegments] = useState<number | null>(null);
  const [inspectorFacts, setInspectorFacts] = useState<number | null>(null);
  const [inspectorLifecycle, setInspectorLifecycle] = useState<DocumentLifecycle | null>(null);

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
  const [webRuns, setWebRuns] = useState<RawWebIngestRunListItem[]>([]);
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runPages, setRunPages] = useState<RawWebIngestRunPage[]>([]);

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
  const statusBadgeConfig = buildDocumentStatusBadgeConfig(t);

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

  const errorMessage = useCallback((error: unknown, fallback: string) => (
    error instanceof Error && error.message ? error.message : fallback
  ), []);

  const editAvailability = useCallback((doc: DocumentItem | null) => {
    if (!doc) {
      return { enabled: false, reason: null as string | null };
    }

    if (!isEditorEditableSourceFormat(doc.fileType)) {
      return { enabled: false, reason: t('documents.editUnavailableFormat') };
    }

    if (
      doc.readiness === 'readable'
      || doc.readiness === 'graph_sparse'
      || doc.readiness === 'graph_ready'
    ) {
      return { enabled: true, reason: null as string | null };
    }

    if (doc.readiness === 'processing') {
      return { enabled: false, reason: t('documents.editUnavailableProcessing') };
    }

    if (doc.readiness === 'failed') {
      return { enabled: false, reason: t('documents.editUnavailableFailed') };
    }

    return { enabled: false, reason: t('documents.editUnavailableGeneric') };
  }, [t]);

  const fetchDocuments = useCallback(async (silent = false) => {
    if (!activeLibrary) return;
    if (!silent) {
      setLoading(true);
      setLoadError(null);
    }
    try {
      const [raw, costs] = await Promise.all([
        documentsApi.list(activeLibrary.id),
        billingApi.getLibraryDocumentCosts(activeLibrary.id).catch(() => []),
      ]);
      const costMap = new Map<string, number>();
      for (const c of costs) {
        costMap.set(c.documentId, parseFloat(c.totalCost));
      }
      const rawList: RawDocumentForUI[] = Array.isArray(raw) ? (raw as RawDocumentForUI[]) : [];
      const items = rawList.map((r) => {
        const doc = mapApiDocument(r, t);
        const cost = costMap.get(doc.id);
        if (cost != null && !isNaN(cost)) {
          doc.cost = cost;
        }
        return doc;
      });
      setDocuments(items);
      // Silently refresh the currently-selected row so the table cells and
      // the inspector header pick up status/cost changes without resetting
      // the user's selection or any inspector sub-state.
      setSelectedDoc((current) => {
        if (!current) return current;
        const refreshed = items.find((item) => item.id === current.id);
        return refreshed ?? current;
      });
      // Also load web ingest runs
      try {
        const runsRaw = await apiFetch<
          RawWebIngestRunListItem[] | RawListEnvelope<RawWebIngestRunListItem>
        >(`/content/web-runs?libraryId=${activeLibrary.id}`);
        const runs = Array.isArray(runsRaw) ? runsRaw : runsRaw?.items ?? [];
        setWebRuns(runs);
      } catch { setWebRuns([]); }
    } catch (err: unknown) {
      if (!silent) {
        setLoadError(errorMessage(err, t('documents.failedToLoad')));
        setDocuments([]);
      }
    } finally {
      if (!silent) setLoading(false);
    }
  }, [activeLibrary, errorMessage, t]);

  useEffect(() => {
    fetchDocuments();
  }, [fetchDocuments]);

  // Quiet background refresh on a fixed cadence. Always runs while the page
  // is mounted so status/cost cells reflect worker progress without a manual
  // reload. `silent=true` keeps the loading spinner and error banner out of
  // the path, and the fetch updater preserves the user's row selection.
  // The selected document's inspector data is refreshed in-place on the same
  // tick so lifecycle stages, costs, segments, and facts stay live without
  // resetting any inspector sub-state.
  useEffect(() => {
    if (!activeLibrary) return;
    const interval = setInterval(() => {
      void fetchDocuments(true);
      const currentId = selectedDocRef.current?.id;
      if (!currentId) return;
      void documentsApi.get(currentId).then((raw) => {
        const refreshed = mapApiDocument(raw as RawDocumentForUI, t);
        setSelectedDoc((current) => (current?.id === refreshed.id ? refreshed : current));
        if (raw.lifecycle) {
          setInspectorLifecycle(raw.lifecycle as DocumentLifecycle);
        }
      }).catch(() => {});
      void Promise.all([
        documentsApi.getPreparedSegments(currentId).catch(() => []),
        documentsApi.getTechnicalFacts(currentId).catch(() => []),
      ]).then(([segments, facts]) => {
        if (selectedDocRef.current?.id !== currentId) return;
        setInspectorSegments(segments.length);
        setInspectorFacts(facts.length);
      });
    }, 5000);
    return () => clearInterval(interval);
  }, [activeLibrary, fetchDocuments, t]);

  const doUploadFile = useCallback(async (file: File) => {
    if (!activeLibrary) return;
    setUploadQueue(prev => [...prev, { name: file.name, state: 'uploading' }]);
    try {
      await documentsApi.upload(activeLibrary.id, file);
      setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'done' } : u));
    } catch (err: unknown) {
      const message = errorMessage(err, t('documents.uploadFailed'));
      setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'error', error: message } : u));
    }
  }, [activeLibrary, errorMessage, t]);

  const doReplaceFile = useCallback(async (docId: string, file: File) => {
    setUploadQueue(prev => [...prev, { name: file.name, state: 'uploading' }]);
    try {
      await documentsApi.replace(docId, file);
      setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'done' } : u));
    } catch (err: unknown) {
      const message = errorMessage(err, t('documents.replaceFileFailed'));
      setUploadQueue(prev => prev.map(u => u.name === file.name ? { ...u, state: 'error', error: message } : u));
    }
  }, [errorMessage, t]);

  const finalizeUpload = useCallback(async () => {
    await fetchDocuments(true);
    setTimeout(() => setUploadQueue([]), 3000);
  }, [fetchDocuments]);

  const processUploadQueue = useCallback(async (files: File[]) => {
    if (!activeLibrary || files.length === 0) { await finalizeUpload(); return; }
    const [file, ...remaining] = files;
    const existing = documents.find(d => d.fileName.toLowerCase() === file.name.toLowerCase());
    if (existing) {
      setDuplicateConflict({ file, existingDocId: existing.id, remaining });
      return;
    }
    await doUploadFile(file);
    await processUploadQueue(remaining);
  }, [activeLibrary, documents, doUploadFile, finalizeUpload]);

  const uploadFiles = useCallback(async (files: File[]) => {
    if (!activeLibrary) return;
    await processUploadQueue(files);
  }, [activeLibrary, processUploadQueue]);

  const handleDuplicateReplace = useCallback(async () => {
    if (!duplicateConflict) return;
    const { file, existingDocId, remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await doReplaceFile(existingDocId, file);
    await processUploadQueue(remaining);
  }, [duplicateConflict, doReplaceFile, processUploadQueue]);

  const handleDuplicateAddNew = useCallback(async () => {
    if (!duplicateConflict) return;
    const { file, remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await doUploadFile(file);
    await processUploadQueue(remaining);
  }, [duplicateConflict, doUploadFile, processUploadQueue]);

  const handleDuplicateSkip = useCallback(async () => {
    if (!duplicateConflict) return;
    const { remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await processUploadQueue(remaining);
  }, [duplicateConflict, processUploadQueue]);

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
    } catch (err: unknown) {
      console.error('Delete failed:', err);
      toast.error(errorMessage(err, t('documents.deleteFailed')));
    }
  }, [errorMessage, fetchDocuments, selectedDoc, t, updateSearchParamState]);

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.reprocess(selectedDoc.id);
      await fetchDocuments();
      // Refresh the selected doc
      const raw = await documentsApi.get(selectedDoc.id);
      setSelectedDoc(mapApiDocument(raw as RawDocumentForUI, t));
    } catch (err: unknown) {
      console.error('Reprocess failed:', err);
      toast.error(errorMessage(err, t('documents.reprocessFailed')));
    }
  }, [errorMessage, fetchDocuments, selectedDoc, t]);

  const handleSelectDoc = useCallback(async (doc: DocumentItem, syncQuery = true) => {
    if (syncQuery) {
      updateSearchParamState({ documentId: doc.id });
    }
    setSelectedDoc(doc);
    setInspectorSegments(null);
    setInspectorFacts(null);
    setInspectorLifecycle(null);
    try {
      const raw = await documentsApi.get(doc.id);
      setSelectedDoc(mapApiDocument(raw as RawDocumentForUI, t));
      if (raw.lifecycle) {
        setInspectorLifecycle(raw.lifecycle as DocumentLifecycle);
      }
    } catch {
      // Keep the list-level data if detail fetch fails
    }
    // Fetch segments and facts counts in parallel
    Promise.all([
      documentsApi.getPreparedSegments(doc.id).catch(() => []),
      documentsApi.getTechnicalFacts(doc.id).catch(() => []),
    ]).then(([segments, facts]) => {
      setInspectorSegments(segments.length);
      setInspectorFacts(facts.length);
    });
  }, [t, updateSearchParamState]);

  useEffect(() => {
    if (!selectedDocumentId) {
      setSelectedDoc(null);
      setInspectorSegments(null);
      setInspectorFacts(null);
      setInspectorLifecycle(null);
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

  const handleReplaceFile = useCallback(async () => {
    if (!selectedDoc || !replaceFile) return;
    setReplaceLoading(true);
    try {
      await documentsApi.replace(selectedDoc.id, replaceFile);
      toast.success(t('documents.replaceFileSuccess'));
      setReplaceFileOpen(false);
      setReplaceFile(null);
      await fetchDocuments();
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('documents.replaceFileFailed')));
    } finally {
      setReplaceLoading(false);
    }
  }, [errorMessage, fetchDocuments, replaceFile, selectedDoc, t]);

  const handleDocumentEditorSaveRefresh = useCallback(async (documentId: string) => {
    await fetchDocuments();
    setInspectorSegments(null);
    setInspectorFacts(null);
    setInspectorLifecycle(null);
    const raw = await documentsApi.get(documentId);
    setSelectedDoc(mapApiDocument(raw as RawDocumentForUI, t));
    if (raw.lifecycle) {
      setInspectorLifecycle(raw.lifecycle as DocumentLifecycle);
    }
  }, [fetchDocuments, t]);

  const documentEditor = useDocumentEditor({
    editAvailability,
    errorMessage,
    onDocumentSaved: handleDocumentEditorSaveRefresh,
    onDocumentSelected: handleSelectDoc,
    selectedDocumentId: selectedDoc?.id ?? null,
    t,
  });

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
    } catch (err: unknown) {
      toast.error(errorMessage(err, t('documents.webIngestFailed')));
    } finally {
      setWebIngestLoading(false);
    }
  }, [activeLibrary, boundaryPolicy, crawlMode, errorMessage, fetchDocuments, maxDepth, maxPages, seedUrl, t]);

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
      const response = await documentsApi.batchReprocess(Array.from(selectedIds));
      const ok = response.reprocessedCount;
      const failed = response.failedCount;
      const skipped = response.skippedCount;
      // Real failures (excluding "already removed" skips) get a destructive
      // toast; skipped-only outcomes get a neutral info toast so they do not
      // alarm the operator. Both are surfaced honestly with counts.
      if (failed === 0 && skipped === 0) {
        toast.success(t('documents.bulkReprocessSuccess', { count: ok }));
      } else if (failed === 0 && skipped > 0) {
        if (ok > 0) {
          toast.success(t('documents.bulkReprocessSkipped', { ok, skipped }));
        } else {
          toast.info(t('documents.bulkReprocessAllSkipped', { skipped }));
        }
      } else if (ok === 0 && skipped === 0) {
        const firstError = response.results.find(r => !r.success && !r.skipped)?.error;
        toast.error(
          t('documents.bulkReprocessAllFailed', {
            count: failed,
            error: firstError ?? '',
          }),
        );
      } else {
        const firstError = response.results.find(r => !r.success && !r.skipped)?.error;
        toast.warning(
          t('documents.bulkReprocessPartial', {
            ok,
            failed,
            skipped,
            error: firstError ?? '',
          }),
        );
      }
      clearSelection();
      await fetchDocuments();
    } catch {
      toast.error(t('documents.bulkReprocessFailed'));
    }
  };

  const processingClockMs = Date.now();

  const filteredDocuments = documents.filter(d => {
    if (searchQuery && !d.fileName.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    if (readinessFilter && d.readiness !== readinessFilter) return false;
    if (statusFilter !== 'all') return documentStatusBucket(d.status) === statusFilter;
    return true;
  }).sort((a, b) => {
    const dir = sortDir === 'asc' ? 1 : -1;
    if (sortField === 'fileName') return a.fileName.localeCompare(b.fileName) * dir;
    if (sortField === 'fileSize') return (a.fileSize - b.fileSize) * dir;
    if (sortField === 'cost') return ((a.cost ?? 0) - (b.cost ?? 0)) * dir;
    if (sortField === 'time') {
      const aTime = getDocumentProcessingDurationMs(a, processingClockMs) ?? 0;
      const bTime = getDocumentProcessingDurationMs(b, processingClockMs) ?? 0;
      return (aTime - bTime) * dir;
    }
    if (sortField === 'finishedAt') {
      // Documents that have not finished yet sort to the bottom in asc order
      // (push "still running" out of the way), and to the top in desc order
      // (most recent finishes at the top, then in-flight, then never-ran).
      const aFinished = a.processingFinishedAt
        ? new Date(a.processingFinishedAt).getTime()
        : null;
      const bFinished = b.processingFinishedAt
        ? new Date(b.processingFinishedAt).getTime()
        : null;
      if (aFinished == null && bFinished == null) return 0;
      if (aFinished == null) return 1; // null sinks regardless of direction
      if (bFinished == null) return -1;
      return (aFinished - bFinished) * dir;
    }
    if (sortField === 'status') {
      const rankDelta = documentStatusSortRank(a.status) - documentStatusSortRank(b.status);
      if (rankDelta !== 0) return rankDelta * dir;
      // Tie-break inside the same status by upload time so the order is
      // deterministic and not jittery between renders.
      return (new Date(a.uploadedAt).getTime() - new Date(b.uploadedAt).getTime()) * dir;
    }
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
    inProgress: documents.filter(d => documentStatusBucket(d.status) === 'in_progress').length,
    attention: documents.filter(d => documentStatusBucket(d.status) === 'attention').length,
    ready: documents.filter(d => documentStatusBucket(d.status) === 'ready').length,
    failed: documents.filter(d => documentStatusBucket(d.status) === 'failed').length,
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
      <DocumentsPageHeader
        activeLibraryName={activeLibrary.name}
        activeTab={activeTab}
        documentsCount={documents.length}
        fileInputRef={fileInputRef}
        handleFileSelect={handleFileSelect}
        setActiveTab={setActiveTab}
        setAddLinkOpen={setAddLinkOpen}
        setBoundaryPolicy={setBoundaryPolicy}
        setCrawlMode={setCrawlMode}
        setMaxDepth={setMaxDepth}
        setMaxPages={setMaxPages}
        setSeedUrl={setSeedUrl}
        t={t}
        uploadQueue={uploadQueue}
        webRunsCount={webRuns.length}
      />
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
              count: statCounts.inProgress,
              icon: <Clock className="h-3 w-3 text-status-processing" />,
              active: statusFilter === 'in_progress',
              updates: { status: 'in_progress', readiness: null, documentId: null, page: null },
            },
            {
              key: 'attention',
              label: t('documents.attention'),
              count: statCounts.attention,
              icon: <AlertTriangle className="h-3 w-3 text-status-stalled" />,
              active: statusFilter === 'attention',
              updates: { status: 'attention', readiness: null, documentId: null, page: null },
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
                        { key: 'time', label: t('documents.pipelineTime') },
                        { key: 'finishedAt', label: t('documents.finished') },
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
                      const rc = statusBadgeConfig[doc.status];
                      const canEditDocument = editAvailability(doc);
                      const typeLabel = formatDocumentTypeLabel(doc.fileType, doc.sourceKind, t);
                      // Tight char limits for the documents table — horizontal
                      // space is at a premium with 8 columns (selection,
                      // name, type, size, uploaded, cost, time, finished,
                      // status). The full name is shown unabbreviated in the
                      // inspector panel on the right when a row is selected.
                      const fileNameLabel = compactText(doc.fileName, 28);
                      const sourceUriLabel = compactText(doc.sourceUri, 36);
                      const processingDurationMs = getDocumentProcessingDurationMs(doc, processingClockMs);
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
                                <span
                                  className="truncate block max-w-[200px] font-semibold"
                                  title={fileNameLabel.fullText}
                                >
                                  {fileNameLabel.text}
                                </span>
                                {doc.sourceKind === 'web_page' && doc.sourceUri && (
                                  <span
                                    className="truncate block max-w-[200px] text-[10px] text-muted-foreground"
                                    title={sourceUriLabel.fullText}
                                  >
                                    {sourceUriLabel.text}
                                  </span>
                                )}
                              </div>
                            </div>
                          </td>
                          <td
                            className={`px-4 py-3.5 text-muted-foreground text-[10px] font-bold tracking-widest ${
                              doc.sourceKind === 'web_page' ? '' : 'uppercase'
                            }`}
                            title={typeLabel}
                          >
                            {typeLabel}
                          </td>
                          <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">{formatSize(doc.fileSize)}</td>
                          <td className="px-4 py-3.5 text-muted-foreground text-xs">{formatDate(doc.uploadedAt, locale)}</td>
                          <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">{doc.cost != null ? `$${doc.cost.toFixed(3)}` : '—'}</td>
                          <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                            {processingDurationMs != null
                              ? `${Math.floor(processingDurationMs / 1000)}s`
                              : '—'}
                          </td>
                          <td className="px-4 py-3.5 text-muted-foreground text-xs">
                            {doc.processingFinishedAt
                              ? formatDate(doc.processingFinishedAt, locale)
                              : '—'}
                          </td>
                          <td className="px-4 py-3.5">
                            <div className="flex items-center gap-2">
                              <span className={`status-badge ${rc.cls}`} title={doc.statusReason}>
                                {rc.label}
                              </span>
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
            const activeRuns = webRuns.filter((r) => !terminalStates.has(r.runState?.toLowerCase()));
            return activeRuns.length > 0 ? (
              <div className="mx-4 mt-4 flex items-center gap-2 text-xs px-3 py-2 rounded-xl bg-card border shadow-soft">
                <Loader2 className="h-3 w-3 animate-spin text-primary" />
                <span className="font-semibold">{t('documents.webRunActiveSummary', { count: activeRuns.length })}</span>
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
                {webRuns.slice(0, 10).map((run) => (
                  <div key={run.runId}>
                    <button
                      className="w-full px-4 py-2.5 flex items-center gap-3 text-left hover:bg-accent/30 transition-colors text-xs"
                      onClick={async () => {
                        if (expandedRunId === run.runId) { setExpandedRunId(null); setRunPages([]); return; }
                        setExpandedRunId(run.runId);
                        try {
                          const pages = await apiFetch<
                            RawWebIngestRunPage[] | RawListEnvelope<RawWebIngestRunPage>
                          >(`/content/web-runs/${run.runId}/pages`);
                          setRunPages(Array.isArray(pages) ? pages : pages?.items ?? []);
                        } catch { setRunPages([]); }
                      }}
                    >
                      <span className={`status-badge ${run.runState === 'completed' ? 'status-ready' : run.runState === 'failed' ? 'status-failed' : 'status-processing'}`}>
                        {run.runState}
                      </span>
                      <span className="truncate font-medium" title={run.seedUrl}>{run.seedUrl}</span>
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
                        {runPages.map((page, i) => (
                          <div key={i} className="flex items-center gap-2 text-[11px]">
                            <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                              page.candidateState === 'processed' ? 'bg-green-500' :
                              page.candidateState === 'failed' ? 'bg-red-500' :
                              page.candidateState === 'excluded' ? 'bg-yellow-500' : 'bg-gray-400'
                            }`} />
                            <span className="truncate text-muted-foreground" title={page.normalizedUrl ?? page.discoveredUrl ?? '?'}>{page.normalizedUrl ?? page.discoveredUrl ?? '?'}</span>
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

        {selectedDoc && (
          <DocumentsInspectorPanel
            canEdit={editAvailability(selectedDoc).enabled}
            editDisabledReason={editAvailability(selectedDoc).reason}
            locale={locale}
            t={t}
            inspectorFacts={inspectorFacts}
            inspectorSegments={inspectorSegments}
            lifecycle={inspectorLifecycle}
            selectedDoc={selectedDoc}
            selectionMode={selectionMode}
            setDeleteDocOpen={setDeleteDocOpen}
            setReplaceFileOpen={setReplaceFileOpen}
            updateSearchParamState={updateSearchParamState}
            onEdit={() => void documentEditor.openEditor(selectedDoc)}
            onRetry={handleRetry}
          />
        )}
      </div>

      <DocumentsOverlays
        activeTab={activeTab}
        addLinkOpen={addLinkOpen}
        boundaryPolicy={boundaryPolicy}
        clearSelection={clearSelection}
        crawlMode={crawlMode}
        deleteDocOpen={deleteDocOpen}
        handleBulkCancel={handleBulkCancel}
        handleBulkDelete={handleBulkDelete}
        handleBulkReprocess={handleBulkReprocess}
        handleDelete={handleDelete}
        handleReplaceFile={handleReplaceFile}
        handleStartWebIngest={handleStartWebIngest}
        maxDepth={maxDepth}
        maxPages={maxPages}
        replaceFile={replaceFile}
        replaceFileInputRef={replaceFileInputRef}
        replaceFileOpen={replaceFileOpen}
        replaceLoading={replaceLoading}
        seedUrl={seedUrl}
        selectedCount={selectedCount}
        selectedDoc={selectedDoc}
        setAddLinkOpen={setAddLinkOpen}
        setBoundaryPolicy={setBoundaryPolicy}
        setCrawlMode={setCrawlMode}
        setDeleteDocOpen={setDeleteDocOpen}
        setMaxDepth={setMaxDepth}
        setMaxPages={setMaxPages}
        setReplaceFile={setReplaceFile}
        setReplaceFileOpen={setReplaceFileOpen}
        setSeedUrl={setSeedUrl}
        t={t}
        webIngestLoading={webIngestLoading}
      />
      <Dialog open={Boolean(duplicateConflict)} onOpenChange={(open) => { if (!open) handleDuplicateSkip(); }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t('documents.duplicateTitle')}</DialogTitle>
            <DialogDescription className="break-all">
              {t('documents.duplicateDescription', { name: duplicateConflict?.file.name ?? '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col gap-2 sm:flex-row">
            <Button variant="default" onClick={handleDuplicateReplace}>
              <RotateCw className="mr-2 h-3.5 w-3.5" /> {t('documents.duplicateReplace')}
            </Button>
            <Button variant="outline" onClick={handleDuplicateAddNew}>
              <Upload className="mr-2 h-3.5 w-3.5" /> {t('documents.duplicateAddNew')}
            </Button>
            <Button variant="ghost" onClick={handleDuplicateSkip}>
              {t('documents.duplicateSkip')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {documentEditor.editorDocument && (
        <DocumentEditorShell
          documentName={documentEditor.editorDocument.fileName}
          error={documentEditor.editorError}
          loading={documentEditor.editorLoading}
          markdown={documentEditor.editorMarkdown}
          onOpenChange={documentEditor.handleEditorOpenChange}
          onSave={documentEditor.saveEditor}
          open={documentEditor.editorOpen}
          saving={documentEditor.editorSaving}
          sourceFormat={documentEditor.editorDocument.fileType}
          t={t}
        />
      )}
    </div>
  );
}
