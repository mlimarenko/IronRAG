import { useState, type FormEvent, type ReactNode } from 'react';
import type { TFunction } from 'i18next';
import {
  Clipboard,
  Eye,
  FilePenLine,
  Download,
  Info,
  Network,
  Pencil,
  RotateCw,
  Trash2,
  Upload,
  X,
  XCircle,
} from 'lucide-react';
import { toast } from 'sonner';

import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/shared/components/ui/tooltip';
import { documentsApi, type DocumentLifecycleDetail } from '@/shared/api';
import type { DocumentItem } from '@/shared/types';
import { compactText, truncatedTitle } from '@/shared/lib/compactText';
import { buildDocumentFailureNotice } from '@/shared/lib/document-processing';

import {
  buildDocumentStatusBadgeConfig,
  formatDate,
  formatDocumentTypeLabel,
  formatSize,
  isWebPageDocument,
} from '@/features/documents/model/documentAdapter';

type DocumentsInspectorPanelProps = {
  canEdit?: boolean;
  canDelete?: boolean;
  documentHintEditable?: boolean;
  editorActionDisabledReason?: string | null;
  editorActionEnabled: boolean;
  editorActionReadOnly?: boolean;
  formatErrorMessage?: (error: unknown, fallback: string) => string;
  locale: string;
  t: TFunction;
  lifecycle: DocumentLifecycleDetail | null;
  selectedDoc: DocumentItem;
  selectionMode: boolean;
  setDeleteDocOpen: (open: boolean) => void;
  setReplaceFileOpen: (open: boolean) => void;
  updateSearchParamState: (updates: Record<string, string | null>) => void;
  onOpenEditor: () => void;
  onDocumentHintUpdated?: (documentId: string, documentHint: string | null) => void;
  onRetry: () => void;
  onViewInGraph?: () => void;
  presentation?: 'sidebar' | 'drawer';
};

const EMPTY_VALUE = '\u2014';

type DocumentHintEditState = {
  documentId: string;
  draft: string;
  editing: boolean;
  saving: boolean;
};

type PipelineStageEvent = DocumentLifecycleDetail['attempts'][number]['stageEvents'][number] & {
  details?: Record<string, unknown> | null;
  providerCallCount?: number | null;
};

type PipelineDetailItem = {
  key: string;
  label: string;
  value: string;
};

type PipelineStageView = {
  costLabel: string | null;
  details: PipelineDetailItem[];
  durationLabel: string;
  event: PipelineStageEvent | null;
  isActive: boolean;
  isCompleted: boolean;
  isFailed: boolean;
  modelLabel: string | null;
  showBilling: boolean;
  stage: string;
};

type InspectorActionButtonProps = {
  label: string;
  icon: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  disabledReason?: string | null;
  variant?: 'default' | 'outline';
  className?: string;
  wrapperClassName?: string;
  tooltipAlign?: 'start' | 'center' | 'end';
};

function InspectorActionButton({
  label,
  icon,
  onClick,
  disabled,
  disabledReason,
  variant = 'outline',
  className = '',
  wrapperClassName = '',
  tooltipAlign = 'center',
}: InspectorActionButtonProps) {
  const tooltipLabel = disabled ? (disabledReason ?? label) : label;
  const tooltipAlignmentClass =
    tooltipAlign === 'start'
      ? 'left-0'
      : tooltipAlign === 'end'
        ? 'right-0'
        : 'left-1/2 -translate-x-1/2';

  return (
    <span className={`group relative inline-flex ${wrapperClassName}`}>
      <Button
        size="icon"
        variant={variant}
        className={`h-8 w-8 rounded-md bg-card shadow-soft [&_svg]:size-4 ${className}`}
        onClick={onClick}
        disabled={disabled}
        aria-label={label}
      >
        {icon}
        <span className="sr-only">{label}</span>
      </Button>
      <span
        role="tooltip"
        className={`pointer-events-none absolute top-full z-30 mt-1 hidden w-max max-w-64 whitespace-normal rounded-md border border-border bg-popover px-2 py-1 text-left text-[10px] font-medium leading-3 text-popover-foreground shadow-lifted group-hover:block group-focus-within:block ${tooltipAlignmentClass}`}
      >
        {tooltipLabel}
      </span>
    </span>
  );
}

const CANONICAL_PIPELINE_STAGES = [
  'extract_content',
  'prepare_structure',
  'chunk_content',
  'extract_technical_facts',
  'embed_chunk',
  'extract_graph',
  'finalizing',
] as const;

function formatPipelineStage(stage: string): string {
  return stage.replace(/_/g, ' ');
}

function formatPipelineDuration(elapsedMs?: number | null): string {
  if (elapsedMs == null) {
    return EMPTY_VALUE;
  }

  return `${(Math.max(0, elapsedMs) / 1000).toFixed(1)}s`;
}

function formatPipelineModel(modelName?: string | null): string | null {
  const trimmed = modelName?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : null;
}

function formatPipelineMoney(
  value?: string | number | null,
  currencyCode?: string | null,
): string | null {
  if (value == null || value === '') {
    return null;
  }

  const amount = Number(value);
  if (!Number.isFinite(amount)) {
    return null;
  }

  const fractionDigits = amount !== 0 && Math.abs(amount) < 0.0001 ? 8 : 4;
  const formattedAmount = amount.toFixed(fractionDigits);
  const currency = currencyCode?.trim().toUpperCase() || 'USD';
  return currency === 'USD' ? `$${formattedAmount}` : `${formattedAmount} ${currency}`;
}

function parsePipelineMoney(value?: string | number | null): number | null {
  if (value == null || value === '') {
    return null;
  }

  const amount = Number(value);
  return Number.isFinite(amount) ? amount : null;
}

function isCompletedStageStatus(status?: string | null): boolean {
  return status === 'completed' || status === 'succeeded' || status === 'ready';
}

function isFailedStageStatus(status?: string | null): boolean {
  return status === 'failed' || status === 'error';
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }

  return value as Record<string, unknown>;
}

function stringDetail(details: Record<string, unknown> | null, key: string): string | null {
  const value = details?.[key];
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function nestedStringDetail(
  details: Record<string, unknown> | null,
  key: string,
  nestedKey: string,
): string | null {
  return stringDetail(asRecord(details?.[key]), nestedKey);
}

function numberDetail(details: Record<string, unknown> | null, key: string): number | null {
  const value = details?.[key];
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === 'string') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }

  return null;
}

function formatIntegerDetail(value: number, locale: string): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value);
}

function formatSourceKind(value: string | null, t: TFunction): string | null {
  if (!value) {
    return null;
  }

  switch (value.trim().toLowerCase()) {
    case 'pdf':
      return 'PDF';
    case 'docx':
      return 'DOCX';
    case 'pptx':
      return 'PPTX';
    case 'image':
      return t('documents.pipelineSourceImage');
    case 'spreadsheet':
      return t('documents.pipelineSourceSpreadsheet');
    case 'text_like':
      return t('documents.pipelineSourceText');
    case 'content_storage':
      return t('documents.pipelineSourceStorage');
    case 'knowledge_revision':
      return t('documents.pipelineSourceRevision');
    default:
      return value;
  }
}

function pushNumberDetail(
  items: PipelineDetailItem[],
  details: Record<string, unknown> | null,
  detailKey: string,
  labelKey: string,
  t: TFunction,
  locale: string,
) {
  const value = numberDetail(details, detailKey);
  if (value == null) {
    return;
  }

  items.push({
    key: detailKey,
    label: t(labelKey),
    value: formatIntegerDetail(value, locale),
  });
}

function buildPipelineDetails(
  stage: PipelineStageEvent,
  t: TFunction,
  locale: string,
): PipelineDetailItem[] {
  const details = asRecord(stage.details);
  const items: PipelineDetailItem[] = [];

  if (stage.stage === 'extract_content') {
    const source = formatSourceKind(
      stringDetail(details, 'fileKind') ?? stringDetail(details, 'source'),
      t,
    );
    if (source) {
      items.push({ key: 'source', label: t('documents.pipelineSource'), value: source });
    }
    const engine = nestedStringDetail(details, 'recognition', 'engine');
    if (engine) {
      items.push({ key: 'engine', label: t('documents.pipelineEngine'), value: engine });
    }
    pushNumberDetail(items, details, 'pageCount', 'documents.pipelinePages', t, locale);
    pushNumberDetail(items, details, 'lineCount', 'documents.pipelineLines', t, locale);
    pushNumberDetail(items, details, 'extractUnitCount', 'documents.pipelineUnits', t, locale);
    pushNumberDetail(
      items,
      details,
      'reusedExtractUnitCount',
      'documents.pipelineReused',
      t,
      locale,
    );
    pushNumberDetail(items, details, 'contentLength', 'documents.pipelineCharacters', t, locale);
  }

  if (stage.stage === 'prepare_structure') {
    pushNumberDetail(items, details, 'blockCount', 'documents.pipelineBlocks', t, locale);
    pushNumberDetail(items, details, 'chunkCount', 'documents.pipelineChunks', t, locale);
  }

  if (stage.stage === 'chunk_content') {
    pushNumberDetail(items, details, 'chunkCount', 'documents.pipelineChunks', t, locale);
  }

  if (stage.stage === 'extract_technical_facts') {
    pushNumberDetail(
      items,
      details,
      'technicalFactCount',
      'documents.pipelineFacts',
      t,
      locale,
    );
    pushNumberDetail(
      items,
      details,
      'technicalConflictCount',
      'documents.pipelineConflicts',
      t,
      locale,
    );
  }

  if (stage.stage === 'embed_chunk') {
    pushNumberDetail(items, details, 'chunksEmbedded', 'documents.pipelineEmbedded', t, locale);
    pushNumberDetail(items, details, 'chunksReused', 'documents.pipelineReused', t, locale);
  }

  if (stage.stage === 'extract_graph') {
    pushNumberDetail(items, details, 'chunksProcessed', 'documents.pipelineChunks', t, locale);
    pushNumberDetail(
      items,
      details,
      'graphChunksSelected',
      'documents.pipelineSelected',
      t,
      locale,
    );
    pushNumberDetail(
      items,
      details,
      'extractedEntityCandidates',
      'documents.pipelineEntities',
      t,
      locale,
    );
    pushNumberDetail(
      items,
      details,
      'extractedRelationCandidates',
      'documents.pipelineRelations',
      t,
      locale,
    );
    pushNumberDetail(items, details, 'projectedNodes', 'documents.pipelineNodes', t, locale);
    pushNumberDetail(items, details, 'projectedEdges', 'documents.pipelineEdges', t, locale);
    pushNumberDetail(items, details, 'reusedChunks', 'documents.pipelineReused', t, locale);
  }

  if (stage.providerCallCount != null && stage.providerCallCount > 0) {
    items.push({
      key: 'providerCallCount',
      label: t('documents.pipelineCalls'),
      value: formatIntegerDetail(stage.providerCallCount, locale),
    });
  }

  return items;
}

export function DocumentsInspectorPanel({
  canEdit = true,
  canDelete = true,
  documentHintEditable = false,
  editorActionDisabledReason,
  editorActionEnabled,
  editorActionReadOnly = false,
  formatErrorMessage,
  locale,
  t,
  lifecycle,
  selectedDoc,
  selectionMode,
  setDeleteDocOpen,
  setReplaceFileOpen,
  updateSearchParamState,
  onOpenEditor,
  onDocumentHintUpdated,
  onRetry,
  onViewInGraph,
  presentation = 'sidebar',
}: DocumentsInspectorPanelProps) {
  const isWebPage = isWebPageDocument(
    selectedDoc.sourceKind,
    selectedDoc.sourceUri,
    selectedDoc.fileName,
  );
  const displayName =
    isWebPage && selectedDoc.sourceUri ? selectedDoc.sourceUri : selectedDoc.fileName;
  const documentHint = selectedDoc.documentHint?.trim() ?? '';
  const [nameExpansion, setNameExpansion] = useState({
    documentId: selectedDoc.id,
    expanded: false,
  });
  const [documentHintTooltipOpen, setDocumentHintTooltipOpen] = useState(false);
  const [documentHintEditState, setDocumentHintEditState] = useState<DocumentHintEditState>({
    documentId: selectedDoc.id,
    draft: documentHint,
    editing: false,
    saving: false,
  });
  const [pipelineSelection, setPipelineSelection] = useState<{
    documentId: string;
    stage: string | null;
  }>({
    documentId: selectedDoc.id,
    stage: null,
  });
  const activeDocumentHintEditState =
    documentHintEditState.documentId === selectedDoc.id
      ? documentHintEditState
      : {
          documentId: selectedDoc.id,
          draft: documentHint,
          editing: false,
          saving: false,
        };
  const documentHintEditing = activeDocumentHintEditState.editing;
  const documentHintDraft = activeDocumentHintEditState.draft;
  const documentHintSaving = activeDocumentHintEditState.saving;

  const showFullName = nameExpansion.documentId === selectedDoc.id && nameExpansion.expanded;
  const compactDisplayName = compactText(displayName, 96);
  const typeLabel = formatDocumentTypeLabel(selectedDoc.fileType, selectedDoc.sourceKind, t, {
    sourceUri: selectedDoc.sourceUri,
    fileName: selectedDoc.fileName,
  });
  const compactTypeLabel = compactText(typeLabel, 54);
  const compactDocumentId = compactText(selectedDoc.id, 30);
  const documentHintDisplay = documentHint.length > 80 ? documentHint.slice(0, 80) : documentHint;
  const documentHintIsUrl =
    documentHint.startsWith('http://') || documentHint.startsWith('https://');
  const showDocumentHintField =
    documentHint.length > 0 || documentHintEditable || documentHintEditing;
  const statusBadge = buildDocumentStatusBadgeConfig(t)[selectedDoc.status];
  const latestLifecycleAttempt = lifecycle?.attempts?.[0];
  const pipelineStageEvents =
    (latestLifecycleAttempt?.stageEvents ?? []) as PipelineStageEvent[];
  const pipelineTotalCost = lifecycle?.totalCost ?? latestLifecycleAttempt?.totalCost;
  const pipelineCurrencyCode = lifecycle?.currencyCode ?? latestLifecycleAttempt?.currencyCode;
  const pipelineTotalDuration = formatPipelineDuration(latestLifecycleAttempt?.totalElapsedMs);
  const pipelineTotalCostLabel = formatPipelineMoney(pipelineTotalCost, pipelineCurrencyCode);
  const showPipelineTotal =
    pipelineTotalDuration !== EMPTY_VALUE || pipelineTotalCostLabel != null;
  const pipelineStageCostByName = new Map<string, { amount: number; currencyCode: string | null }>();
  for (const attempt of lifecycle?.attempts ?? []) {
    for (const stageEvent of attempt.stageEvents ?? []) {
      const amount = parsePipelineMoney(stageEvent.estimatedCost);
      if (amount == null) {
        continue;
      }
      const existing = pipelineStageCostByName.get(stageEvent.stage);
      pipelineStageCostByName.set(stageEvent.stage, {
        amount: (existing?.amount ?? 0) + amount,
        currencyCode:
          existing?.currencyCode ??
          stageEvent.currencyCode ??
          attempt.currencyCode ??
          pipelineCurrencyCode ??
          null,
      });
    }
  }
  const failureNotice =
    selectedDoc.status === 'failed'
      ? selectedDoc.failureNotice ??
        buildDocumentFailureNotice(
          {
            failureCode: selectedDoc.failureCode,
            failureMessage: selectedDoc.failureMessage ?? selectedDoc.statusReason,
            stage: selectedDoc.stage,
          },
          t,
        )
      : undefined;
  const visibleProgressPercent =
    selectedDoc.progressPercent != null
      ? selectedDoc.progressPercent
      : selectedDoc.status === 'processing'
        ? 0
        : null;
  const showInspectorProgress = visibleProgressPercent != null && selectedDoc.status !== 'ready';
  const pipelineStageByName = new Map<string, PipelineStageEvent>();
  for (const stageEvent of pipelineStageEvents) {
    pipelineStageByName.set(stageEvent.stage, stageEvent);
  }
  const pipelineStageNames = [
    ...CANONICAL_PIPELINE_STAGES,
    ...pipelineStageEvents
      .map((stageEvent) => stageEvent.stage)
      .filter((stage) => !(CANONICAL_PIPELINE_STAGES as readonly string[]).includes(stage)),
  ];
  const rawPipelineStageViews = pipelineStageNames.map((stage) => {
    const event = pipelineStageByName.get(stage) ?? null;
    const modelLabel = formatPipelineModel(event?.modelName);
    const stageCurrency = event?.currencyCode ?? lifecycle?.currencyCode;
    const aggregateCost = pipelineStageCostByName.get(stage);
    const costLabel = aggregateCost
      ? formatPipelineMoney(aggregateCost.amount, aggregateCost.currencyCode)
      : formatPipelineMoney(event?.estimatedCost, stageCurrency);
    const durationLabel = formatPipelineDuration(event?.elapsedMs);
    const details = event ? buildPipelineDetails(event, t, locale) : [];

    return {
      costLabel,
      details,
      durationLabel,
      event,
      isActive: false,
      isCompleted: isCompletedStageStatus(event?.status),
      isFailed: isFailedStageStatus(event?.status),
      modelLabel,
      showBilling: modelLabel != null || costLabel != null,
      stage,
    };
  });
  const failedPipelineStage = rawPipelineStageViews.find((stage) => stage.isFailed);
  const livePipelineStage = rawPipelineStageViews.find(
    (stage) => stage.event && !stage.isCompleted && !stage.isFailed,
  );
  const lastObservedPipelineStageIndex = rawPipelineStageViews.reduce(
    (lastIndex, stage, index) => (stage.event ? index : lastIndex),
    -1,
  );
  const currentPipelineStageName =
    failedPipelineStage?.stage ??
    livePipelineStage?.stage ??
    (selectedDoc.status === 'processing' || selectedDoc.status === 'queued'
      ? rawPipelineStageViews[
          Math.min(Math.max(lastObservedPipelineStageIndex + 1, 0), rawPipelineStageViews.length - 1)
        ]?.stage
      : null);
  const pipelineStageViews: PipelineStageView[] = rawPipelineStageViews.map((stage) => ({
    ...stage,
    isActive: stage.stage === currentPipelineStageName,
  }));
  const selectedPipelineStageName =
    pipelineSelection.documentId === selectedDoc.id ? pipelineSelection.stage : null;
  const focusedPipelineStageName = selectedPipelineStageName ?? currentPipelineStageName ?? null;
  const focusedPipelineStage = focusedPipelineStageName
    ? (pipelineStageViews.find((stage) => stage.stage === focusedPipelineStageName) ?? null)
    : null;
  const rootClassName =
    presentation === 'drawer'
      ? 'h-full overflow-y-auto bg-card'
      : 'inspector-panel w-80 lg:w-96 shrink-0 hidden md:block overflow-y-auto animate-slide-in-right';

  const openSource = () => {
    const href = selectedDoc.sourceAccess?.href ?? selectedDoc.sourceUri;
    if (!href) {
      return;
    }

    window.open(href, '_blank', 'noopener,noreferrer');
  };

  const openDocumentHintEditor = () => {
    if (!documentHintEditable) {
      return;
    }
    setDocumentHintEditState({
      documentId: selectedDoc.id,
      draft: documentHint,
      editing: true,
      saving: false,
    });
  };

  const cancelDocumentHintEdit = () => {
    setDocumentHintEditState({
      documentId: selectedDoc.id,
      draft: documentHint,
      editing: false,
      saving: false,
    });
  };

  const saveDocumentHint = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!documentHintEditable || documentHintSaving) {
      return;
    }

    const nextDocumentHint = documentHintDraft.trim();
    const normalizedDocumentHint = nextDocumentHint.length > 0 ? nextDocumentHint : null;
    setDocumentHintEditState({
      documentId: selectedDoc.id,
      draft: documentHintDraft,
      editing: true,
      saving: true,
    });
    try {
      const savedDocumentHint = await documentsApi.updateDocumentHint(
        selectedDoc.id,
        normalizedDocumentHint,
      );
      onDocumentHintUpdated?.(selectedDoc.id, savedDocumentHint);
      setDocumentHintEditState({
        documentId: selectedDoc.id,
        draft: savedDocumentHint ?? '',
        editing: false,
        saving: false,
      });
      toast.success(t('documents.documentHintUpdated'));
    } catch (error) {
      toast.error(
        formatErrorMessage
          ? formatErrorMessage(error, t('documents.documentHintUpdateFailed'))
          : t('documents.documentHintUpdateFailed'),
      );
    } finally {
      setDocumentHintEditState((state) =>
        state.documentId === selectedDoc.id
          ? { ...state, saving: false }
          : state,
      );
    }
  };

  const editorActionLabel = editorActionReadOnly ? t('documents.viewDocument') : t('documents.edit');
  const editorActionIcon = editorActionReadOnly ? <Eye /> : <FilePenLine />;
  const sourceActionLabel =
    selectedDoc.sourceAccess?.kind === 'stored_document'
      ? t('documents.downloadDocument')
      : t('documents.openSourceUrl');
  const retryActionLabel = t('documents.retryProcessing');
  const replaceActionLabel = t('documents.replaceFile');
  const deleteActionLabel = t('documents.delete');
  const hasSourceAction = Boolean(selectedDoc.sourceAccess?.href || selectedDoc.sourceUri);

  return (
    <div
      className={`${rootClassName} ${
        selectionMode ? 'opacity-40 pointer-events-none' : ''
      }`}
    >
      <div className="border-b px-4 py-3 flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h3
            className="text-sm font-bold tracking-tight leading-5 [overflow-wrap:anywhere]"
            title={compactDisplayName.isTruncated && !showFullName ? displayName : undefined}
          >
            {showFullName || !compactDisplayName.isTruncated
              ? displayName
              : compactDisplayName.text}
          </h3>
          {compactDisplayName.isTruncated && (
            <button
              type="button"
              className="mt-1 text-xs font-medium text-primary hover:text-primary/80"
              onClick={() =>
                setNameExpansion((value) => ({
                  documentId: selectedDoc.id,
                  expanded: value.documentId === selectedDoc.id ? !value.expanded : true,
                }))
              }
            >
              {showFullName ? t('documents.showLessName') : t('documents.showFullName')}
            </button>
          )}
        </div>
        <button
          onClick={() => updateSearchParamState({ documentId: null })}
          className="shrink-0 p-1.5 rounded-lg hover:bg-muted transition-colors"
          aria-label={t('common.close')}
        >
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="p-3 space-y-3">
        <div className={showInspectorProgress ? 'space-y-1' : undefined}>
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <span className={`status-badge ${statusBadge.cls} whitespace-nowrap`} title={selectedDoc.statusReason}>
                {statusBadge.label}
              </span>
              {selectedDoc.stage &&
                (selectedDoc.status === 'processing' || selectedDoc.status === 'queued') && (
                  <span className="ml-2 text-xs text-muted-foreground">{selectedDoc.stage}</span>
                )}
            </div>
            {showInspectorProgress && (
              <span className="shrink-0 text-xs font-medium tabular-nums">{visibleProgressPercent}%</span>
            )}
          </div>
          {showInspectorProgress && (
            <div
              className="h-1.5 bg-surface-sunken rounded-full overflow-hidden"
              style={{ boxShadow: 'inset 0 1px 2px hsl(var(--foreground) / 0.04)' }}
            >
              <div
                className="h-full bg-primary rounded-full transition-all duration-500"
                style={{
                  width: `${visibleProgressPercent}%`,
                  boxShadow: '0 0 8px -2px hsl(var(--primary) / 0.4)',
                }}
              />
            </div>
          )}
        </div>

        {failureNotice && (
          <div className="inline-error" data-testid="document-failure-notice">
            <div className="flex items-start gap-1.5 [overflow-wrap:anywhere]">
              <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive" />
              <div className="min-w-0 space-y-1">
                <div className="font-semibold">{failureNotice.title}</div>
                <div>{failureNotice.summary}</div>
                <div className="text-muted-foreground">{failureNotice.impact}</div>
              </div>
            </div>
            <div className="mt-2 rounded-md border border-destructive/20 bg-destructive/10 px-2 py-1.5 text-xs">
              <div className="font-semibold text-foreground">{t('documents.failureActionLabel')}</div>
              <div className="mt-0.5 text-muted-foreground [overflow-wrap:anywhere]">
                {failureNotice.action}
              </div>
            </div>
            {(failureNotice.diagnosticMessage || failureNotice.diagnosticCode) && (
              <div className="mt-2 space-y-1.5">
                <div className="section-label">{t('documents.failureDiagnostics')}</div>
                {failureNotice.diagnosticMessage && (
                  <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    {failureNotice.diagnosticMessage}
                  </div>
                )}
                {failureNotice.diagnosticCode && (
                  <div className="flex items-start gap-1.5">
                    <code className="min-w-0 flex-1 font-mono text-[10px] text-muted-foreground [overflow-wrap:anywhere]">
                      {failureNotice.diagnosticCode}
                    </code>
                    <button
                      type="button"
                      className="shrink-0 p-0.5 rounded hover:bg-muted/60 transition-colors text-muted-foreground hover:text-foreground"
                      aria-label={t('documents.copyFailureCode')}
                      onClick={() => {
                        void navigator.clipboard
                          .writeText(failureNotice.diagnosticCode ?? '')
                          .then(() => toast.success(t('documents.failureCodeCopied')));
                      }}
                    >
                      <Clipboard className="h-3 w-3" />
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {/* Source link is available via the download button in actions */}

        <div className="space-y-1.5">
          <div className="section-label">
            {isWebPage ? t('documents.webSource') : t('documents.fileInfo')}
          </div>
          <div className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
            {[
              {
                label: t('documents.type'),
                value: compactTypeLabel.text,
                title: truncatedTitle(compactTypeLabel),
              },
              { label: t('documents.size'), value: formatSize(selectedDoc.fileSize) },
              { label: t('documents.uploaded'), value: formatDate(selectedDoc.uploadedAt, locale) },
              ...(selectedDoc.externalKey
                ? [
                    {
                      label: t('documents.externalKey'),
                      value: selectedDoc.externalKey,
                      title: selectedDoc.externalKey,
                    },
                  ]
                : []),
            ].map((item) => (
              <div key={item.label} className="min-w-0">
                <div className="truncate leading-4 text-muted-foreground">{item.label}</div>
                <div
                  className="truncate font-mono text-xs font-semibold leading-4 text-foreground"
                  title={item.title}
                >
                  {item.value}
                </div>
              </div>
            ))}
            {/* Document ID with copy button */}
            <div className="col-span-2 min-w-0">
              <div className="truncate leading-4 text-muted-foreground">{t('documents.documentId')}</div>
              <div className="flex items-center gap-1.5 min-w-0">
                <span
                  className="truncate font-mono text-xs font-semibold leading-4 text-foreground"
                  title={selectedDoc.id}
                >
                  {compactDocumentId.text}
                </span>
                <button
                  type="button"
                  className="shrink-0 p-0.5 rounded hover:bg-muted transition-colors text-muted-foreground hover:text-foreground"
                  aria-label={t('documents.copyDocumentId')}
                  onClick={() => {
                    void navigator.clipboard.writeText(selectedDoc.id).then(() =>
                      toast.success(t('documents.documentIdCopied')),
                    );
                  }}
                >
                  <Clipboard className="h-3 w-3" />
                </button>
              </div>
            </div>
            {showDocumentHintField && (
              <div className="col-span-2 min-w-0">
                <div className="flex min-w-0 items-center gap-1">
                  <div className="truncate leading-4 text-muted-foreground">
                    {t('documents.documentHint')}
                  </div>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <button
                        type="button"
                        className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                        aria-label={t('documents.documentHintTooltip')}
                      >
                        <Info className="h-3.5 w-3.5" />
                      </button>
                    </TooltipTrigger>
                    <TooltipContent align="start" className="max-w-64">
                      {t('documents.documentHintTooltip')}
                    </TooltipContent>
                  </Tooltip>
                </div>
                {documentHintEditing ? (
                  <form className="mt-1 flex min-w-0 items-center gap-1.5" onSubmit={saveDocumentHint}>
                    <Input
                      aria-label={t('documents.documentHint')}
                      autoFocus
                      className="h-8 min-w-0 flex-1 rounded-md px-2.5 font-mono text-xs"
                      disabled={documentHintSaving}
                      onChange={(event) =>
                        setDocumentHintEditState({
                          documentId: selectedDoc.id,
                          draft: event.target.value,
                          editing: true,
                          saving: false,
                        })
                      }
                      placeholder={t('documents.documentHintEditPlaceholder')}
                      value={documentHintDraft}
                    />
                    <Button
                      className="h-8 rounded-md px-2.5 text-xs"
                      disabled={documentHintSaving}
                      size="sm"
                      type="submit"
                    >
                      {t('documents.documentHintEditSave')}
                    </Button>
                    <Button
                      className="h-8 rounded-md px-2.5 text-xs"
                      disabled={documentHintSaving}
                      onClick={cancelDocumentHintEdit}
                      size="sm"
                      type="button"
                      variant="outline"
                    >
                      {t('documents.documentHintEditCancel')}
                    </Button>
                  </form>
                ) : (
                  <div className="mt-0.5 flex min-w-0 items-center gap-1.5">
                    {documentHintEditable ? (
                      <button
                        type="button"
                        className="min-w-0 flex-1 truncate text-left font-mono text-xs font-semibold leading-4 text-foreground underline-offset-2 hover:text-primary hover:underline"
                        onClick={openDocumentHintEditor}
                        title={documentHint || t('documents.documentHintEditPlaceholder')}
                      >
                        {documentHint ? documentHintDisplay : EMPTY_VALUE}
                      </button>
                    ) : documentHintIsUrl ? (
                      <a
                        className="min-w-0 truncate font-mono text-xs font-semibold leading-4 text-primary underline-offset-2 hover:underline"
                        href={documentHint}
                        rel="noopener noreferrer"
                        target="_blank"
                        title={documentHint}
                      >
                        {documentHintDisplay}
                      </a>
                    ) : (
                      <div
                        className="min-w-0 truncate font-mono text-xs font-semibold leading-4 text-foreground"
                        title={documentHint}
                      >
                        {documentHintDisplay}
                      </div>
                    )}
                    {documentHintEditable && (
                      <Button
                        aria-label={t('documents.documentHintEditAria')}
                        className="h-6 w-6 shrink-0 rounded-md"
                        onClick={openDocumentHintEditor}
                        size="icon"
                        type="button"
                        variant="ghost"
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>

        {pipelineStageEvents.length > 0 && (
          <div className="space-y-2">
            <div className="section-label">{t('documents.pipeline')}</div>
            <div className="space-y-1.5" data-testid="document-pipeline">
              <div className="overflow-hidden rounded-md border border-border/70 bg-background">
                {pipelineStageViews.map((stage) => {
                  const isSelected = stage.stage === focusedPipelineStage?.stage;
                  const rowTone = stage.isFailed
                    ? 'border-l-destructive bg-destructive/5 text-destructive'
                    : stage.isActive
                      ? 'border-l-primary bg-primary/[0.055] text-primary'
                      : isSelected
                        ? 'border-l-primary/45 bg-surface-sunken/55 text-foreground'
                        : 'border-l-transparent text-foreground hover:bg-surface-sunken/45';
                  const dotTone = stage.isFailed
                    ? 'bg-destructive'
                    : stage.isActive
                      ? 'bg-primary'
                      : stage.isCompleted
                        ? 'bg-emerald-500'
                        : 'bg-muted-foreground/35';
                  const showFocusedDetails =
                    isSelected &&
                    (stage.showBilling ||
                      stage.details.length > 0 ||
                      stage.durationLabel !== EMPTY_VALUE);

                  return (
                    <div key={stage.stage} className="border-t border-border/55 first:border-t-0">
                      <button
                        type="button"
                        data-pipeline-stage={stage.stage}
                        data-testid={`pipeline-stage-tab-${stage.stage}`}
                        aria-current={stage.isActive ? 'step' : undefined}
                        aria-expanded={showFocusedDetails}
                        className={`grid w-full grid-cols-[minmax(0,1fr)_2.8rem_minmax(4.6rem,auto)] items-center gap-2 border-l-2 px-2.5 py-1 text-left text-[11px] leading-4 transition-colors ${rowTone}`}
                        onClick={() =>
                          setPipelineSelection({
                            documentId: selectedDoc.id,
                            stage: stage.stage,
                          })
                        }
                      >
                        <span className="flex min-w-0 items-center gap-1.5">
                          <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${dotTone}`} />
                          <span className="min-w-0 truncate font-semibold capitalize">
                            {formatPipelineStage(stage.stage)}
                          </span>
                        </span>
                        <span className="text-right tabular-nums text-muted-foreground">
                          {stage.durationLabel !== EMPTY_VALUE ? stage.durationLabel : EMPTY_VALUE}
                        </span>
                        <span className="text-right font-semibold tabular-nums text-foreground">
                          {stage.costLabel ?? ''}
                        </span>
                      </button>

                      {showFocusedDetails && (
                        <div
                          data-testid={`pipeline-stage-${stage.stage}`}
                          className={`max-h-20 overflow-y-auto border-l-2 border-t px-3 py-1.5 ${
                            stage.isFailed
                              ? 'border-l-destructive border-t-destructive/15 bg-destructive/[0.035]'
                              : stage.isActive
                                ? 'border-l-primary border-t-primary/15 bg-primary/[0.035]'
                                : 'border-l-primary/35 border-t-border/55 bg-surface-sunken/35'
                          }`}
                        >
                          {stage.showBilling && (
                            <div className="flex min-w-0 items-center gap-2 rounded bg-background/75 px-2 py-1 text-[11px] leading-4 ring-1 ring-border/45">
                              {stage.modelLabel != null && (
                                <span
                                  className="min-w-0 truncate font-mono text-[10px] text-foreground"
                                  title={stage.modelLabel}
                                >
                                  {stage.modelLabel}
                                </span>
                              )}
                              {stage.costLabel != null && (
                                <span className="ml-auto shrink-0 whitespace-nowrap font-semibold tabular-nums">
                                  {stage.costLabel}
                                </span>
                              )}
                            </div>
                          )}
                          {stage.details.length > 0 && (
                            <div className={`${stage.showBilling ? 'mt-1.5' : ''} flex flex-wrap gap-1`}>
                              {stage.details.map((item) => (
                                <span
                                  key={item.key}
                                  className="inline-flex min-w-0 items-center gap-1 rounded bg-background/70 px-1.5 py-0.5 text-[11px] leading-4 text-muted-foreground ring-1 ring-border/40"
                                >
                                  <span>{item.label}</span>
                                  <span className="font-medium tabular-nums text-foreground [overflow-wrap:anywhere]">
                                    {item.value}
                                  </span>
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>

              {showPipelineTotal && (
                <div className="flex items-center justify-between gap-3 rounded-md border border-border bg-surface-sunken/45 px-3 py-1 text-xs font-semibold">
                  <span>{t('documents.pipelineTotal')}</span>
                  <div className="flex shrink-0 items-center gap-3">
                    {pipelineTotalDuration !== EMPTY_VALUE && (
                      <span className="tabular-nums text-muted-foreground">
                        {pipelineTotalDuration}
                      </span>
                    )}
                    {pipelineTotalCostLabel != null && (
                      <span className="tabular-nums">{pipelineTotalCostLabel}</span>
                    )}
                  </div>
                </div>
              )}
            </div>
          </div>
        )}

        <div className="flex items-center gap-1.5 pt-0.5 flex-wrap">
          <InspectorActionButton
            label={editorActionLabel}
            icon={editorActionIcon}
            onClick={onOpenEditor}
            disabled={!editorActionEnabled}
            disabledReason={editorActionDisabledReason}
            variant="default"
            className="bg-primary"
            tooltipAlign="start"
          />
          {hasSourceAction && (
            <InspectorActionButton
              label={sourceActionLabel}
              icon={<Download />}
              onClick={openSource}
            />
          )}
          {onViewInGraph && (
            <InspectorActionButton
              label={t('documents.viewInGraph')}
              icon={<Network />}
              onClick={onViewInGraph}
            />
          )}
          {canEdit && (
            <InspectorActionButton
              label={retryActionLabel}
              icon={<RotateCw />}
              onClick={onRetry}
            />
          )}
          {canEdit && (
            <InspectorActionButton
              label={replaceActionLabel}
              icon={<Upload />}
              onClick={() => setReplaceFileOpen(true)}
            />
          )}
          {canDelete && (
            <InspectorActionButton
              label={deleteActionLabel}
              icon={<Trash2 />}
              onClick={() => setDeleteDocOpen(true)}
              className="text-destructive hover:text-destructive"
              wrapperClassName="ml-auto"
              tooltipAlign="end"
            />
          )}
        </div>

      </div>
    </div>
  );
}
