import type { WebIngestPattern, WebIngestUrlFilter } from "@/shared/api/admin";

const PATTERN_KINDS: readonly WebIngestPattern["kind"][] = [
  "url_prefix",
  "path_prefix",
  "glob",
];

function isWebIngestPatternKind(value: string): value is WebIngestPattern["kind"] {
  return PATTERN_KINDS.some((kind) => kind === value);
}

function inferPatternKind(value: string): WebIngestPattern["kind"] {
  if (/^https?:\/\//i.test(value)) {
    return "url_prefix";
  }
  if (value.startsWith("/")) {
    return "path_prefix";
  }
  return "glob";
}

function validatePatternValue(
  kind: WebIngestPattern["kind"],
  value: string,
): string {
  const normalized = value.trim();
  if (!normalized) {
    throw new Error(`${kind} value is empty`);
  }
  if (normalized.length > 2048) {
    throw new Error(`${kind} value is longer than 2048 characters`);
  }
  if (kind === "path_prefix" && !normalized.startsWith("/")) {
    throw new Error("path_prefix value must start with /");
  }
  if (kind === "url_prefix") {
    try {
      new URL(normalized);
    } catch {
      throw new Error("url_prefix value must be an absolute URL");
    }
  }
  return normalized;
}

export function formatWebIngestPatterns(
  patterns: WebIngestPattern[] | undefined,
): string {
  return (patterns ?? [])
    .map((pattern) => `${pattern.kind}:${pattern.value}`)
    .join("\n");
}

export function parseWebIngestPatternText(
  text: string,
): WebIngestPattern[] {
  const seen = new Set<string>();
  const patterns: WebIngestPattern[] = [];
  for (const line of text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)) {
    const separator = line.indexOf(":");
    let kind: WebIngestPattern["kind"];
    let value: string;
    if (separator > 0 && !/^https?:\/\//i.test(line)) {
      const rawKind = line.slice(0, separator).trim();
      if (!isWebIngestPatternKind(rawKind)) {
        throw new Error(`${rawKind} is not a supported pattern kind`);
      }
      kind = rawKind;
      value = line.slice(separator + 1).trim();
    } else {
      kind = inferPatternKind(line);
      value = line;
    }
    const normalizedValue = validatePatternValue(kind, value);
    const dedupeKey = `${kind}\u0000${normalizedValue}`;
    if (!seen.has(dedupeKey)) {
      seen.add(dedupeKey);
      patterns.push({ kind, value: normalizedValue });
    }
  }
  return patterns;
}

export function buildWebIngestUrlFilter(
  allowText: string,
  blockText: string,
): WebIngestUrlFilter {
  return {
    allowPatterns: parseWebIngestPatternText(allowText),
    blockPatterns: parseWebIngestPatternText(blockText),
  };
}

export type WebIngestFilterMatchKind = "allow" | "block";

export type WebIngestFilterEvaluation =
  | {
      status: "invalid_url";
      passes: false;
      normalizedUrl: null;
      matchKind: null;
      matchedPattern: null;
    }
  | {
      status: "blocked";
      passes: false;
      normalizedUrl: string;
      matchKind: "block";
      matchedPattern: WebIngestPattern;
    }
  | {
      status: "no_allow_match";
      passes: false;
      normalizedUrl: string;
      matchKind: null;
      matchedPattern: null;
    }
  | {
      status: "allowed";
      passes: true;
      normalizedUrl: string;
      matchKind: "allow";
      matchedPattern: WebIngestPattern;
    }
  | {
      status: "allowed_without_rules";
      passes: true;
      normalizedUrl: string;
      matchKind: null;
      matchedPattern: null;
    };

type EvaluationUrl = {
  parsedUrl: URL;
  url: string;
};

function normalizeEvaluationUrl(value: string): EvaluationUrl | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const candidate = /^https?:\/\//i.test(trimmed)
    ? trimmed
    : `https://${trimmed}`;
  try {
    return { parsedUrl: new URL(candidate), url: candidate };
  } catch {
    return null;
  }
}

function wildcardMatches(pattern: string, candidate: string): boolean {
  let patternIndex = 0;
  let candidateIndex = 0;
  let starIndex = -1;
  let starCandidateIndex = 0;

  while (candidateIndex < candidate.length) {
    if (
      patternIndex < pattern.length &&
      (pattern[patternIndex] === candidate[candidateIndex] ||
        pattern[patternIndex] === "?")
    ) {
      patternIndex += 1;
      candidateIndex += 1;
    } else if (patternIndex < pattern.length && pattern[patternIndex] === "*") {
      starIndex = patternIndex;
      patternIndex += 1;
      starCandidateIndex = candidateIndex;
    } else if (starIndex >= 0) {
      patternIndex = starIndex + 1;
      starCandidateIndex += 1;
      candidateIndex = starCandidateIndex;
    } else {
      return false;
    }
  }

  while (patternIndex < pattern.length && pattern[patternIndex] === "*") {
    patternIndex += 1;
  }

  return patternIndex === pattern.length;
}

function matchWebIngestPattern(
  url: string,
  parsedUrl: URL,
  pattern: WebIngestPattern,
): boolean {
  switch (pattern.kind) {
    case "url_prefix":
      return url.startsWith(pattern.value);
    case "path_prefix":
      return parsedUrl.pathname.startsWith(pattern.value);
    case "glob":
      return wildcardMatches(pattern.value, url);
    default:
      return false;
  }
}

function findMatchedPattern(
  url: string,
  parsedUrl: URL,
  patterns: WebIngestPattern[],
): WebIngestPattern | null {
  return (
    patterns.find((pattern) => matchWebIngestPattern(url, parsedUrl, pattern)) ??
    null
  );
}

export function formatWebIngestPattern(pattern: WebIngestPattern): string {
  return `${pattern.kind}:${pattern.value}`;
}

export function evaluateWebIngestUrlFilter(
  urlText: string,
  filter: WebIngestUrlFilter,
): WebIngestFilterEvaluation {
  const evaluationUrl = normalizeEvaluationUrl(urlText);
  if (evaluationUrl == null) {
    return {
      status: "invalid_url",
      passes: false,
      normalizedUrl: null,
      matchKind: null,
      matchedPattern: null,
    };
  }
  const { parsedUrl, url } = evaluationUrl;

  const blockedPattern = findMatchedPattern(url, parsedUrl, filter.blockPatterns);
  if (blockedPattern) {
    return {
      status: "blocked",
      passes: false,
      normalizedUrl: url,
      matchKind: "block",
      matchedPattern: blockedPattern,
    };
  }

  if (filter.allowPatterns.length === 0) {
    return {
      status: "allowed_without_rules",
      passes: true,
      normalizedUrl: url,
      matchKind: null,
      matchedPattern: null,
    };
  }

  const allowedPattern = findMatchedPattern(url, parsedUrl, filter.allowPatterns);
  if (allowedPattern) {
    return {
      status: "allowed",
      passes: true,
      normalizedUrl: url,
      matchKind: "allow",
      matchedPattern: allowedPattern,
    };
  }

  return {
    status: "no_allow_match",
    passes: false,
    normalizedUrl: url,
    matchKind: null,
    matchedPattern: null,
  };
}
