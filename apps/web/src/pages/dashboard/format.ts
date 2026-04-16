import type { TFunction } from 'i18next';
import type { DocumentReadiness } from '@/types';
import type {
  DashboardAttentionItem,
  DashboardMetric,
  GraphStatus,
  MessageLevel,
  WebIngestRunState,
} from './types';

export function formatRelativeTime(iso: string, locale: string): string {
  const timestamp = new Date(iso).getTime();
  if (Number.isNaN(timestamp)) return iso;

  const diffSeconds = Math.round((timestamp - Date.now()) / 1000);
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });

  if (Math.abs(diffSeconds) < 60) return formatter.format(diffSeconds, 'second');

  const diffMinutes = Math.round(diffSeconds / 60);
  if (Math.abs(diffMinutes) < 60) return formatter.format(diffMinutes, 'minute');

  const diffHours = Math.round(diffMinutes / 60);
  if (Math.abs(diffHours) < 24) return formatter.format(diffHours, 'hour');

  const diffDays = Math.round(diffHours / 24);
  return formatter.format(diffDays, 'day');
}

export function formatDateTime(
  iso: string | null | undefined,
  locale: string,
  emptyLabel: string,
): string {
  if (!iso) return emptyLabel;

  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return emptyLabel;

  return new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date);
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function metricValue(
  metrics: DashboardMetric[],
  key: string,
  fallback = 0,
): number {
  const value = Number(metrics.find((metric) => metric.key === key)?.value ?? fallback);
  return Number.isFinite(value) ? value : fallback;
}

export function readinessClass(readiness: DocumentReadiness): string {
  switch (readiness) {
    case 'graph_ready':
      return 'status-ready';
    case 'graph_sparse':
      return 'status-warning';
    case 'failed':
      return 'status-failed';
    case 'readable':
    case 'processing':
    default:
      return 'status-processing';
  }
}

export function attentionClass(level: MessageLevel): string {
  switch (level) {
    case 'error':
      return 'status-failed';
    case 'warning':
      return 'status-warning';
    case 'info':
    default:
      return 'status-processing';
  }
}

export function graphStatusClass(status: GraphStatus): string {
  switch (status) {
    case 'ready':
      return 'status-ready';
    case 'partial':
    case 'stale':
    case 'building':
    case 'rebuilding':
      return 'status-warning';
    case 'failed':
      return 'status-failed';
    case 'empty':
    default:
      return 'status-processing';
  }
}

export function runStateClass(state: WebIngestRunState): string {
  switch (state) {
    case 'completed':
      return 'status-ready';
    case 'completed_partial':
    case 'discovering':
    case 'accepted':
    case 'processing':
      return 'status-warning';
    case 'failed':
    case 'canceled':
      return 'status-failed';
    default:
      return 'status-processing';
  }
}

export type ToneKey = 'neutral' | 'ready' | 'warning' | 'processing' | 'failed';

export function toneStyle(tone: ToneKey) {
  if (tone === 'neutral') {
    return {
      container: { background: 'hsl(var(--muted))' },
      iconClass: 'text-muted-foreground',
    };
  }

  return {
    container: {
      background: `hsl(var(--status-${tone}-bg))`,
      boxShadow: `inset 0 0 0 1px hsl(var(--status-${tone}-ring) / 0.35)`,
    },
    iconClass:
      tone === 'ready'
        ? 'text-status-ready'
        : tone === 'warning'
          ? 'text-status-warning'
          : tone === 'failed'
            ? 'text-status-failed'
            : 'text-status-processing',
  };
}

export function hostnameFromUrl(value: string): string {
  try {
    return new URL(value).hostname;
  } catch {
    return value;
  }
}

/**
 * Attention-item codes are a closed, i18n-backed enum shared with the
 * backend. When the code is known, localize from the i18n bundle so the
 * message is controlled by the translator; when it is a backend-minted
 * `other`/unknown code, pass through the raw title/detail the backend sent.
 */
export function localizeAttention(
  item: DashboardAttentionItem,
  t: TFunction,
): { title: string; detail: string } {
  switch (item.code) {
    case 'failed_documents':
    case 'graph_sparse':
    case 'graph_coverage_gap':
    case 'retryable_document':
    case 'stale_vectors':
    case 'stale_relations':
    case 'failed_rebuilds':
    case 'bundle_assembly_failures':
      return {
        title: t(`dashboard.attentionTitles.${item.code}`),
        detail: t(`dashboard.attentionDetails.${item.code}`),
      };
    default:
      return { title: item.title, detail: item.detail };
  }
}

/**
 * Resolve the destination route for an attention item. Unknown codes fall
 * back to the backend-provided `routePath` so new backend categories keep
 * working without a frontend bump.
 */
export function resolveAttentionRoute(
  item: DashboardAttentionItem,
  graphCoverageActionPath: string,
): string {
  switch (item.code) {
    case 'failed_documents':
    case 'retryable_document':
    case 'failed_rebuilds':
    case 'stale_vectors':
    case 'graph_sparse':
      return '/documents';
    case 'graph_coverage_gap':
      return graphCoverageActionPath;
    case 'stale_relations':
    case 'bundle_assembly_failures':
      return '/graph';
    default:
      return item.routePath;
  }
}
