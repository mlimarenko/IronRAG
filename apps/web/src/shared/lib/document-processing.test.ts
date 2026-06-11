import { describe, expect, it } from 'vitest';

import i18n from '@/shared/i18n';

import {
  buildDocumentFailureNotice,
  buildUploadFailureNotice,
  humanizeDocumentFailure,
} from './document-processing';

const t = i18n.t.bind(i18n);

describe('document-processing failure presenters', () => {
  it('builds clear guidance for upload extraction failures without using the raw code as the message', () => {
    const notice = buildDocumentFailureNotice(
      {
        failureCode: 'upload_extraction_failed',
        failureMessage: 'Upload extraction failed',
        stage: 'extract_content',
      },
      t,
    );

    expect(notice?.title).toBe('Processing failed');
    expect(notice?.summary).toContain('could not be extracted');
    expect(notice?.summary).not.toBe('upload_extraction_failed');
    expect(notice?.action).toContain('Retry the upload');
    expect(notice?.diagnosticCode).toBe('upload_extraction_failed');
    expect(notice?.diagnosticMessage).toBe('Upload extraction failed');
  });

  it('normalizes dynamic compound batch codes while retaining the full diagnostic code', () => {
    const notice = buildDocumentFailureNotice(
      {
        failureCode: 'children_failed:2/10',
        failureMessage: null,
      },
      t,
    );

    expect(notice?.summary).toBe('Some documents in the batch operation failed.');
    expect(notice?.action).toContain('Open the failed documents');
    expect(notice?.diagnosticCode).toBe('children_failed:2/10');
  });

  it('uses known code guidance even when the backend message is empty', () => {
    const notice = buildDocumentFailureNotice(
      {
        failureCode: 'stored_source_unavailable',
        failureMessage: '',
      },
      t,
    );

    expect(notice?.summary).toBe('The source file could not be read.');
    expect(notice?.action).toContain('Retry after storage is healthy');
  });

  it.each([
    ['graph_reconcile_timeout', 'Graph reconciliation exceeded its time limit.'],
    ['provider_timeout', 'The AI provider did not respond before the timeout.'],
    ['batch_timeout', 'The batch operation exceeded the time limit.'],
  ])('keeps specific timeout summaries reachable for %s', (code, expectedSummary) => {
    const notice = buildDocumentFailureNotice(
      {
        failureCode: code,
        failureMessage: null,
      },
      t,
    );

    expect(notice?.summary).toBe(expectedSummary);
  });

  it('falls back to a readable unknown-code message and generic recovery action', () => {
    const notice = buildDocumentFailureNotice(
      {
        failureCode: 'worker_pool_exhausted:3/4',
        failureMessage: null,
      },
      t,
    );

    expect(notice?.summary).toBe('Processing failed: Worker pool exhausted.');
    expect(notice?.action).toContain('Retry processing');
    expect(notice?.diagnosticCode).toBe('worker_pool_exhausted:3/4');
  });

  it('uses a known code explanation before raw backend diagnostics', () => {
    expect(
      humanizeDocumentFailure(
        {
          failureCode: 'parser_failed',
          stalledReason: 'Parser failed on page 2',
        },
        t,
      ),
    ).toBe('The document parser could not extract usable content.');
  });

  it('keeps non-code backend messages when no known code is available', () => {
    expect(
      humanizeDocumentFailure(
        {
          stalledReason: 'Parser failed on page 2',
        },
        t,
      ),
    ).toBe('Parser failed on page 2');
  });

  it('prefers localized upload rejection guidance while keeping API details as diagnostics', () => {
    const notice = buildUploadFailureNotice(
      {
        body: {
          error: 'invalid file body for upload.bin',
          errorKind: 'invalid_file_body',
          details: {
            rejectionCause: 'The upload stream ended before the file body was complete.',
            operatorAction: 'Retry the upload or upload the file individually.',
          },
        },
      },
      'Upload failed',
      t,
    );

    expect(notice.summary).toBe('The uploaded file body could not be read completely.');
    expect(notice.action).toBe(
      'Retry the upload. If it keeps failing, upload the file individually to isolate the broken file body.',
    );
    expect(notice.diagnosticCode).toBe('invalid_file_body');
    expect(notice.diagnosticMessage).toBe(
      'invalid file body for upload.bin | The upload stream ended before the file body was complete. | Retry the upload or upload the file individually.',
    );
  });
});
