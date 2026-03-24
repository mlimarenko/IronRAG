function baseResult(issues, extra = {}) {
  return {
    pass: issues.length === 0,
    issues,
    ...extra,
  };
}

export function validateCapabilities(capabilitiesResult) {
  const issues = [];
  if (!capabilitiesResult || capabilitiesResult.status !== 200) {
    issues.push(`capabilities status must be 200 (got ${capabilitiesResult?.status ?? "n/a"})`);
  }
  return baseResult(issues);
}

export function validateInitialize(initResult) {
  const issues = [];
  if (initResult?.error) {
    issues.push("initialize returned error");
  }
  if (initResult?.data?.result == null) {
    issues.push("initialize result is missing");
  }
  return baseResult(issues);
}

export function validateToolsList(toolsListResult) {
  const issues = [];
  const tools = toolsListResult?.data?.result?.tools;
  if (!Array.isArray(tools)) {
    issues.push("tools/list result.tools must be an array");
  }
  return baseResult(issues);
}

export function validateSearch(searchResult) {
  const issues = [];
  const isError = searchResult?.data?.isError;
  if (isError !== false) {
    issues.push("search result indicates error");
  }
  const hitCount = Array.isArray(searchResult?.data?.result?.structuredContent?.hits)
    ? searchResult.data.result.structuredContent.hits.length
    : Array.isArray(searchResult?.data?.result?.content)
      ? searchResult.data.result.content.length
      : 0;
  return baseResult(issues, { hitCount });
}

export function validateUpload(uploadResult) {
  const issues = [];
  if (uploadResult?.data?.isError !== false) {
    issues.push("upload result indicates error");
  }
  return baseResult(issues);
}

export function validateReadback(readResult) {
  const issues = [];
  if (readResult?.data?.isError !== false) {
    issues.push("readback result indicates error");
  }
  const content = readResult?.data?.result?.content ?? readResult?.data?.result?.structuredContent?.content;
  const hasContent = Array.isArray(content) ? content.length > 0 : content != null && String(content).trim() !== "";
  if (!hasContent) {
    issues.push("readback content is missing");
  }
  return baseResult(issues);
}

export function validateReplaySafety(result1, result2) {
  const issues = [];
  const hasData1 = result1?.data != null;
  const hasData2 = result2?.data != null;
  if (hasData1 !== hasData2) {
    issues.push("response envelope mismatch");
  }
  const isError1 = result1?.data?.isError;
  const isError2 = result2?.data?.isError;
  if (isError1 !== isError2) {
    issues.push("isError mismatch between replayed calls");
  }
  return baseResult(issues);
}
