const CODE_CATEGORY_RULES = [
  { needle: "timeout", category: "extraction_timeout" },
  { needle: "unsupported", category: "unsupported_format" },
  { needle: "empty", category: "content_empty" },
  { needle: "provider", category: "provider_error" },
];

const MESSAGE_CATEGORY_RULES = [
  { needle: "timed out", category: "extraction_timeout" },
  { needle: "timeout", category: "extraction_timeout" },
  { needle: "unsupported", category: "unsupported_format" },
  { needle: "not supported", category: "unsupported_format" },
  { needle: "empty content", category: "content_empty" },
  { needle: "no content", category: "content_empty" },
  { needle: "provider", category: "provider_error" },
  { needle: "upstream", category: "provider_error" },
];

function messageForCategory(category) {
  switch (category) {
    case "extraction_timeout":
      return "Extraction timed out. Retry or reduce document complexity.";
    case "unsupported_format":
      return "Document format is unsupported. Use a supported file type.";
    case "content_empty":
      return "No extractable content was found. Verify file contents.";
    case "provider_error":
      return "Provider-side processing failed. Retry after provider recovers.";
    default:
      return "Ingestion failed for an unknown reason. Inspect attempt details.";
  }
}

function categorizeByNeedle(value, rules) {
  const normalized = String(value ?? "").toLowerCase();
  for (const rule of rules) {
    if (normalized.includes(rule.needle)) {
      return rule.category;
    }
  }
  return null;
}

export function classifyIngestionFailure(failureCode, failureMessage) {
  const byCode = categorizeByNeedle(failureCode, CODE_CATEGORY_RULES);
  const byMessage = categorizeByNeedle(failureMessage, MESSAGE_CATEGORY_RULES);
  const category = byCode ?? byMessage ?? "unknown";
  return {
    category,
    operatorMessage: messageForCategory(category),
  };
}
