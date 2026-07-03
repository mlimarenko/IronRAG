import type { TFunction } from 'i18next';
import type { StatusTone } from '@/shared/components/StatusBadge';
import type { DocumentReadiness } from '@/shared/types';
import type {
  DashboardAttentionItem,
  GraphStatus,
  MessageLevel,
  WebIngestRunState,
} from './types';

/**
 * Strips the `status-` prefix so a `status-*` class name (as produced by
 * `readinessClass` / `attentionClass` / `graphStatusClass` / `runStateClass`)
 * can be passed as the canonical `<StatusBadge tone>` prop.
 */
export function toStatusTone(statusClass: string): StatusTone {
  return statusClass.replace(/^status-/, '') as StatusTone;
}

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

export function toneClass(tone: ToneKey): { containerClass: string; iconClass: string } {
  switch (tone) {
    case 'ready':
      return { containerClass: 'bg-status-ready-bg ring-1 ring-status-ready-ring/35', iconClass: 'text-status-ready' };
    case 'warning':
      return { containerClass: 'bg-status-warning-bg ring-1 ring-status-warning-ring/35', iconClass: 'text-status-warning' };
    case 'failed':
      return { containerClass: 'bg-status-failed-bg ring-1 ring-status-failed-ring/35', iconClass: 'text-status-failed' };
    case 'processing':
      return { containerClass: 'bg-status-processing-bg ring-1 ring-status-processing-ring/35', iconClass: 'text-status-processing' };
    case 'neutral':
    default:
      return { containerClass: 'bg-muted', iconClass: 'text-muted-foreground' };
  }
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
): { title: string; detail: string; action: string } {
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
        action: t(`dashboard.attentionActions.${item.code}`),
      };
    default:
      return {
        title: item.title,
        detail: item.detail,
        action: t('dashboard.attentionActions.default'),
      };
  }
}

/**
 * Attention destinations are backend-owned. The frontend only keeps the
 * navigation inside the app shell and rejects empty/external routes.
 */
export function resolveAttentionRoute(item: DashboardAttentionItem): string {
  const route = item.routePath?.trim();
  if (!route || !route.startsWith('/') || route.startsWith('//')) {
    return '/dashboard';
  }
  return route;
}
