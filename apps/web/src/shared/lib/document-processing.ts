import type { TFunction } from 'i18next';

function normalizeToken(value: string | null | undefined): string | null {
  const normalized = value?.trim().toLowerCase();
  return normalized ? normalized : null;
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
  if (value.length === 0) {
    return value;
  }

  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

function isCodeLike(value: string): boolean {
  return /^[a-z0-9_:-]+$/i.test(value.trim());
}

function stageFailureMessage(stage: string | null | undefined, t: TFunction): string | undefined {
  const normalizedStage = normalizeToken(stage);
  if (!normalizedStage) {
    return undefined;
  }

  return i18nValue(`documents.failureMessages.byStage.${normalizedStage}`, t);
}

function codeFailureMessage(code: string | null | undefined, t: TFunction): string | undefined {
  const normalizedCode = normalizeToken(code);
  if (!normalizedCode) {
    return undefined;
  }

  return i18nValue(`documents.failureMessages.byCode.${normalizedCode}`, t);
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

export function humanizeDocumentFailure(
  input: {
    failureCode?: string | null;
    stalledReason?: string | null;
    stage?: string | null;
  },
  t: TFunction,
): string | undefined {
  const rawReason = input.stalledReason?.trim();
  const normalizedReason = normalizeToken(rawReason);
  const normalizedCode = normalizeToken(input.failureCode);

  if (
    normalizedReason &&
    normalizedReason.includes('knowledge context bundle')
  ) {
    return codeFailureMessage('knowledge_context_bundle_failed', t);
  }

  if (normalizedReason?.includes('timeout') || normalizedCode?.includes('timeout')) {
    return codeFailureMessage('timeout', t);
  }

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
      code: sentenceCase(prettifyToken(rawToken)),
    });
  }

  return i18nValue('documents.failureMessages.generic', t);
}
