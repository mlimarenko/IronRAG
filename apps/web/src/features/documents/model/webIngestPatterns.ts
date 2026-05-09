import type { WebIngestPattern } from "@/shared/api/admin";

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
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const separator = line.indexOf(":");
      if (separator > 0) {
        const rawKind = line.slice(0, separator).trim();
        const value = line.slice(separator + 1).trim();
        if (isWebIngestPatternKind(rawKind)) {
          if (!value) {
            throw new Error(`${rawKind} value is empty`);
          }
          return { kind: rawKind, value };
        }
      }
      return { kind: inferPatternKind(line), value: line };
    });
}
