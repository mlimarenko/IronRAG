import type { TFunction } from 'i18next';

function normalizeToken(value: string | null | undefined): string | null {
  const normalized = value?.trim().toLowerCase();
  return normalized ? normalized : null;
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

function translatedOrNull(t: TFunction, key: string, options?: Record<string, unknown>): string | null {
  const translated = t(key, { defaultValue: '', ...options });
  return translated ? String(translated) : null;
}

function stageFailureMessage(stage: string | null | undefined, t: TFunction): string | undefined {
  const normalizedStage = normalizeToken(stage);
  if (!normalizedStage) {
    return undefined;
  }

  return translatedOrNull(t, `documents.failureMessages.byStage.${normalizedStage}`) ?? undefined;
}

function codeFailureMessage(code: string | null | undefined, t: TFunction): string | undefined {
  const normalizedCode = normalizeToken(code);
  if (!normalizedCode) {
    return undefined;
  }

  return translatedOrNull(t, `documents.failureMessages.byCode.${normalizedCode}`) ?? undefined;
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
    translatedOrNull(t, `documents.stageLabels.${normalizedStage}`) ??
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

  return translatedOrNull(t, 'documents.failureMessages.generic') ?? undefined;
}
