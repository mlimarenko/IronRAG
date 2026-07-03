import type { TFunction } from 'i18next';

function normalizeToken(value: string | null | undefined): string | null {
  const normalized = value?.trim().toLowerCase();
  return normalized ? normalized : null;
}

function normalizeLookupKey(value: string | null | undefined): string | null {
  const normalized = normalizeToken(value);
  if (!normalized) {
    return null;
  }

  return normalized.split(':', 1)[0] ?? normalized;
}

/**
 * Look up a translation key without a defaultValue fallback.
 * i18next returns the key itself when no translation exists, so
 * we treat key-echo as a miss and return undefined.
 */
function i18nValue(key: string, t: TFunction): string | undefined {
  const result = t(key);
  return result !== key ? result : undefined;
}

function prettifyToken(value: string): string {
  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

function sentenceCase(value: string): string {
  const first = value[0];
  if (!first) {
    return value;
  }

  return `${first.toUpperCase()}${value.slice(1)}`;
}

function isCodeLike(value: string): boolean {
  return /^[a-z0-9_:-]+$/i.test(value.trim());
}

function stageFailureMessage(stage: string | null | undefined, t: TFunction): string | undefined {
  const normalizedStage = normalizeLookupKey(stage);
  if (!normalizedStage) {
    return undefined;
  }

  return i18nValue(`documents.failureMessages.byStage.${normalizedStage}`, t);
}

function stageFailureAction(stage: string | null | undefined, t: TFunction): string | undefined {
  const normalizedStage = normalizeLookupKey(stage);
  if (!normalizedStage) {
    return undefined;
  }

  return i18nValue(`documents.failureActions.byStage.${normalizedStage}`, t);
}

function codeFailureMessage(code: string | null | undefined, t: TFunction): string | undefined {
  const normalizedCode = normalizeLookupKey(code);
  if (!normalizedCode) {
    return undefined;
  }

  return i18nValue(`documents.failureMessages.byCode.${normalizedCode}`, t);
}

function codeFailureAction(code: string | null | undefined, t: TFunction): string | undefined {
  const normalizedCode = normalizeLookupKey(code);
  if (!normalizedCode) {
    return undefined;
  }

  return i18nValue(`documents.failureActions.byCode.${normalizedCode}`, t);
}

export function humanizeDocumentStage(
  stage: string | null | undefined,
  t: TFunction,
): string | undefined {
  const normalizedStage = normalizeToken(stage);
  if (!normalizedStage) {
    return undefined;
  }

  return (
    i18nValue(`documents.stageLabels.${normalizedStage}`, t) ||
    sentenceCase(prettifyToken(normalizedStage))
  );
}

export type DocumentFailureNotice = {
  title: string;
  summary: string;
  impact: string;
  action: string;
  diagnosticCode?: string | undefined;
  diagnosticMessage?: string | undefined;
};

export type UploadFailureNotice = {
  summary: string;
  action: string;
  diagnosticCode?: string | undefined;
  diagnosticMessage?: string | undefined;
};

function recordValue(value: unknown, key: string): unknown {
  return value && typeof value === 'object' ? (value as Record<string, unknown>)[key] : undefined;
}

function stringRecordValue(value: unknown, key: string): string | undefined {
  const candidate = recordValue(value, key);
  return typeof candidate === 'string' && candidate.trim() ? candidate.trim() : undefined;
}

export function humanizeDocumentFailure(
  input: {
    failureCode?: string | null | undefined;
    stalledReason?: string | null | undefined;
    stage?: string | null | undefined;
  },
  t: TFunction,
): string | undefined {
  const rawReason = input.stalledReason?.trim();
  const normalizedReason = normalizeToken(rawReason);
  const normalizedCode = normalizeToken(input.failureCode);

  if (normalizedCode === 'canonical_pipeline_failed') {
    const stageSpecific = stageFailureMessage(input.stage, t);
    if (stageSpecific) {
      return stageSpecific;
    }
  }

  const codeMessage = codeFailureMessage(input.failureCode, t);
  if (codeMessage) {
    return codeMessage;
  }

  if (normalizedCode?.includes('timeout')) {
    return codeFailureMessage('timeout', t);
  }

  const reasonCodeMessage = codeFailureMessage(rawReason, t);
  if (reasonCodeMessage) {
    return reasonCodeMessage;
  }

  if (rawReason && !isCodeLike(rawReason)) {
    return rawReason;
  }

  const fallbackStageMessage = stageFailureMessage(input.stage, t);
  if (fallbackStageMessage) {
    return fallbackStageMessage;
  }

  const rawToken = normalizedCode ?? normalizedReason;
  if (rawToken) {
    return t('documents.failureMessages.unknownCode', {
      code: sentenceCase(prettifyToken(normalizeLookupKey(rawToken) ?? rawToken)),
    });
  }

  return i18nValue('documents.failureMessages.generic', t);
}

export function buildDocumentFailureNotice(
  input: {
    failureCode?: string | null | undefined;
    failureMessage?: string | null | undefined;
    stage?: string | null | undefined;
  },
  t: TFunction,
): DocumentFailureNotice | undefined {
  const summary = humanizeDocumentFailure(
    {
      failureCode: input.failureCode,
      stalledReason: input.failureMessage,
      stage: input.stage,
    },
    t,
  );
  if (!summary) {
    return undefined;
  }

  const action =
    codeFailureAction(input.failureCode, t) ??
    (input.failureCode === 'canonical_pipeline_failed'
      ? stageFailureAction(input.stage, t)
      : undefined) ??
    stageFailureAction(input.stage, t) ??
    i18nValue('documents.failureActions.generic', t) ??
    '';
  const rawMessage = input.failureMessage?.trim();
  const diagnosticMessage =
    rawMessage && rawMessage !== summary && rawMessage !== input.failureCode
      ? rawMessage
      : undefined;
  const diagnosticCode = input.failureCode?.trim() || undefined;

  return {
    title: t('documents.failureNoticeTitle'),
    summary,
    impact: t('documents.failureImpact'),
    action,
    diagnosticCode,
    diagnosticMessage,
  };
}

export function buildUploadFailureNotice(
  error: unknown,
  fallback: string,
  t: TFunction,
): UploadFailureNotice {
  const body = recordValue(error, 'body');
  const details = recordValue(body, 'details');
  const errorKind =
    stringRecordValue(body, 'errorKind') ??
    stringRecordValue(body, 'error_kind') ??
    stringRecordValue(body, 'code');
  const rejectionCause =
    stringRecordValue(details, 'rejectionCause') ??
    stringRecordValue(details, 'rejection_cause');
  const operatorAction =
    stringRecordValue(details, 'operatorAction') ??
    stringRecordValue(details, 'operator_action');
  const apiMessage =
    stringRecordValue(body, 'error') ??
    stringRecordValue(body, 'message') ??
    (error instanceof Error ? error.message : undefined);
  const codeSummary = humanizeDocumentFailure(
    {
      failureCode: errorKind,
      stalledReason: rejectionCause ?? apiMessage,
    },
    t,
  );
  const codeAction = codeFailureAction(errorKind, t);
  const summary = codeSummary ?? rejectionCause ?? apiMessage ?? fallback;
  const action =
    codeAction ??
    operatorAction ??
    i18nValue('documents.failureActions.uploadGeneric', t) ??
    i18nValue('documents.failureActions.generic', t) ??
    fallback;
  const diagnosticMessage = [apiMessage, rejectionCause, operatorAction]
    .filter((value): value is string => Boolean(value))
    .filter((value, index, values) => values.indexOf(value) === index)
    .filter((value) => value !== summary && value !== action && value !== errorKind)
    .join(' | ') || undefined;

  return {
    summary,
    action,
    diagnosticCode: errorKind,
    diagnosticMessage,
  };
}
