export function evaluateGraphSemantics(entities, expectedTerms, minMatchedTerms) {
  const labels = entities
    .map((item) =>
      String(
        item?.canonical_label ??
          item?.displayLabel ??
          item?.label ??
          item?.name ??
          "",
      ).toLowerCase(),
    )
    .filter(Boolean);
  const matchedTerms = expectedTerms.filter((term) =>
    labels.some((label) => label.includes(term.toLowerCase())),
  );
  return {
    matchedTerms,
    matchedCount: matchedTerms.length,
    expectedCount: expectedTerms.length,
    semanticPass: matchedTerms.length >= minMatchedTerms,
  };
}
