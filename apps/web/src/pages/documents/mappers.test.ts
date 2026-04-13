import { describe, expect, it } from 'vitest';

import i18n from '@/i18n';
import type { DocumentItem } from '@/types';

import { formatDocumentTypeLabel, getDocumentProcessingDurationMs, mapApiDocument } from './mappers';

describe('formatDocumentTypeLabel', () => {
  it('renders a canonical web page label for web-ingested documents', () => {
    expect(formatDocumentTypeLabel('php', 'web_page', i18n.t.bind(i18n))).toBe('Web page');
  });

  it('keeps extension-driven labels for uploaded documents', () => {
    expect(formatDocumentTypeLabel('xlsx', 'upload', i18n.t.bind(i18n))).toBe('XLSX');
  });
});

describe('getDocumentProcessingDurationMs', () => {
  function buildDocument(overrides: Partial<DocumentItem> = {}): DocumentItem {
    return {
      id: 'doc-1',
      fileName: 'inventory.xlsx',
      fileType: 'xlsx',
      fileSize: 2048,
      uploadedAt: '2026-04-10T12:00:00Z',
      cost: null,
      status: 'processing',
      readiness: 'processing',
      processingStartedAt: '2026-04-10T12:00:05Z',
      ...overrides,
    };
  }

  it('ticks from claimed_at through now while the worker holds the job', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument(),
      Date.parse('2026-04-10T12:01:05Z'),
    );

    expect(durationMs).toBe(60_000);
  });

  it('returns null for queued documents so the UI does not accrue idle seconds', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'queued',
        readiness: 'processing',
        processingStartedAt: undefined,
      }),
      Date.parse('2026-04-10T12:05:00Z'),
    );

    expect(durationMs).toBeNull();
  });

  it('uses finished_at once the job has completed', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'ready',
        readiness: 'graph_ready',
        processingStartedAt: '2026-04-10T12:00:05Z',
        processingFinishedAt: '2026-04-10T12:00:45Z',
      }),
    );

    expect(durationMs).toBe(40_000);
  });

  it('clamps inverted timestamps instead of returning a negative duration', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'ready',
        readiness: 'graph_ready',
        processingStartedAt: '2026-04-10T12:05:00Z',
        processingFinishedAt: '2026-04-10T12:04:00Z',
      }),
    );

    expect(durationMs).toBe(0);
  });
});

describe('mapApiDocument', () => {
  const baseRaw = {
    fileName: 'inventory.xlsx',
    document: {
      id: 'doc-1',
      external_key: 'inventory',
      created_at: '2026-04-10T12:00:00Z',
    },
    activeRevision: {
      title: 'inventory.xlsx',
      mime_type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      byte_size: 2048,
      content_source_kind: 'upload',
    },
  } as const;

  it('marks queued documents without a processing start so the timer stays hidden', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'processing', activityStatus: 'queued' },
        pipeline: {
          latest_job: {
            queue_state: 'queued',
            queued_at: '2026-04-10T12:00:03Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('queued');
    expect(doc.readiness).toBe('processing');
    expect(doc.processingStartedAt).toBeUndefined();
    expect(doc.processingFinishedAt).toBeUndefined();
    expect(getDocumentProcessingDurationMs(doc)).toBeNull();
  });

  it('marks leased documents as processing with a start from claimed_at', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'processing', activityStatus: 'active' },
        pipeline: {
          latest_job: {
            queue_state: 'leased',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('processing');
    expect(doc.processingStartedAt).toBe('2026-04-10T12:00:10Z');
    expect(doc.processingFinishedAt).toBeUndefined();
  });

  it('maps completed jobs onto the readiness-derived terminal status', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'graph_ready', activityStatus: 'ready' },
        pipeline: {
          latest_job: {
            queue_state: 'completed',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            completed_at: '2026-04-10T12:01:03Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('ready');
    expect(doc.readiness).toBe('graph_ready');
    expect(doc.processingStartedAt).toBe('2026-04-10T12:00:10Z');
    expect(doc.processingFinishedAt).toBe('2026-04-10T12:01:03Z');
    expect(getDocumentProcessingDurationMs(doc)).toBe(53_000);
  });

  it('routes failed jobs to a failed status regardless of readiness', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'processing', activityStatus: 'failed' },
        pipeline: {
          latest_job: {
            queue_state: 'failed',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            failure_code: 'llm_timeout',
            retryable: true,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('failed');
    expect(doc.readiness).toBe('failed');
    expect(doc.canRetry).toBe(true);
  });

  it('flags a stalled worker when the backend reports activity_status=stalled', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: {
          readinessKind: 'processing',
          activityStatus: 'stalled',
          stalledReason: 'no visible activity for 300s',
        },
        pipeline: {
          latest_job: {
            queue_state: 'leased',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            current_stage: 'extract_graph',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('stalled');
    expect(doc.readiness).toBe('processing');
    expect(doc.statusReason).toContain('no visible activity for 300s');
    expect(doc.processingStartedAt).toBe('2026-04-10T12:00:10Z');
    // Timer keeps ticking for stalled jobs so the user sees how long it has been stuck.
    expect(getDocumentProcessingDurationMs(doc, Date.parse('2026-04-10T12:05:10Z'))).toBe(300_000);
  });

  it('flags a blocked dependency wait as blocked with the reason surfaced', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: {
          readinessKind: 'processing',
          activityStatus: 'blocked',
          stalledReason: 'blocked on upstream bundle',
        },
        pipeline: {
          latest_job: {
            queue_state: 'leased',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('blocked');
    expect(doc.statusReason).toContain('blocked on upstream bundle');
  });

  it('flags retrying attempts distinctly only when the worker is actively holding the job', () => {
    // `activity_status='retrying'` only matters while `queue_state='leased'`.
    // A queued retry-attempt is just "queued" from the user's POV — no
    // worker has picked it up yet — and the canonical mapper reports it as
    // such instead of letting `activity_status` leak through.
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: {
          readinessKind: 'processing',
          activityStatus: 'retrying',
        },
        pipeline: {
          latest_job: {
            queue_state: 'leased',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            retryable: true,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('retrying');
  });

  it('classifies a completed job that produced no readable content as failed (zombie)', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'processing', activityStatus: 'stalled' },
        pipeline: {
          latest_job: {
            queue_state: 'completed',
            queued_at: '2026-04-10T12:00:03Z',
            claimed_at: '2026-04-10T12:00:10Z',
            completed_at: '2026-04-10T12:00:30Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    // Job is terminal but readiness never reached `ready`/`graph_ready`/etc.
    // The previous mapper would put this in `stalled` (Needs Attention) via
    // activity_status; the canonical mapper recognizes it as a terminal
    // failure and routes it to the Failed bucket with an explanatory reason.
    expect(doc.status).toBe('failed');
    expect(doc.statusReason).toContain('readable');
  });

  it('keeps a queued doc in the queued bucket even if a stale claim leaks activity_status=stalled', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'processing', activityStatus: 'stalled' },
        pipeline: {
          latest_job: {
            queue_state: 'queued',
            queued_at: '2026-04-10T12:00:03Z',
            // No `claimed_at` because the previous attempt was force-reset.
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('queued');
  });

  it('keeps a doc with already-readable content as ready even if more work is queued', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'readable', activityStatus: 'queued' },
        pipeline: {
          latest_job: {
            queue_state: 'queued',
            queued_at: '2026-04-10T12:00:03Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('ready');
  });

  it('distinguishes a manually canceled job from a real failure', () => {
    const doc = mapApiDocument(
      {
        ...baseRaw,
        readinessSummary: { readinessKind: 'failed', activityStatus: 'failed' },
        pipeline: {
          latest_job: {
            queue_state: 'canceled',
            queued_at: '2026-04-10T12:00:03Z',
            retryable: false,
          },
        },
      },
      i18n.t.bind(i18n),
    );

    expect(doc.status).toBe('canceled');
    expect(doc.failureMessage).toBeUndefined();
  });
});
