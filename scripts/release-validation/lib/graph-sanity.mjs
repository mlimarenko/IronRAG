function normalizedText(value) {
  return String(value ?? "").trim().toLowerCase();
}

function relationSubjectId(relation) {
  return (
    relation?.subject_entity_id ??
    relation?.subjectEntityId ??
    relation?.subject?.id ??
    relation?.subject_id ??
    null
  );
}

function relationObjectId(relation) {
  return (
    relation?.object_entity_id ??
    relation?.objectEntityId ??
    relation?.object?.id ??
    relation?.object_id ??
    null
  );
}

export function checkRelationSanity(relations) {
  const issues = [];
  const assertions = new Set();
  for (const relation of Array.isArray(relations) ? relations : []) {
    const relationId = relation?.id ?? relation?.relation_id ?? "unknown";
    const predicate = normalizedText(
      relation?.predicate ?? relation?.relation ?? relation?.verb ?? relation?.type,
    );
    if (!predicate) {
      issues.push(`relation ${relationId} has empty predicate`);
    }

    const subjectId = relationSubjectId(relation);
    const objectId = relationObjectId(relation);
    if (subjectId != null && objectId != null && String(subjectId) === String(objectId)) {
      issues.push(`relation ${relationId} is a self-loop`);
    }

    const normalizedAssertion = normalizedText(relation?.normalized_assertion);
    if (normalizedAssertion) {
      if (assertions.has(normalizedAssertion)) {
        issues.push(`duplicate normalized_assertion: ${normalizedAssertion}`);
      } else {
        assertions.add(normalizedAssertion);
      }
    }
  }
  return { issues };
}

function getLibraryId(item) {
  return item?.library_id ?? item?.libraryId ?? null;
}

export function checkOwnershipIsolation(entities, relations, expectedLibraryId) {
  const issues = [];
  const expected = String(expectedLibraryId);

  for (const entity of Array.isArray(entities) ? entities : []) {
    const actual = getLibraryId(entity);
    if (actual != null && String(actual) !== expected) {
      issues.push(`entity ${entity?.id ?? "unknown"} belongs to library ${String(actual)}`);
    }
  }
  for (const relation of Array.isArray(relations) ? relations : []) {
    const actual = getLibraryId(relation);
    if (actual != null && String(actual) !== expected) {
      issues.push(`relation ${relation?.id ?? "unknown"} belongs to library ${String(actual)}`);
    }
  }
  return { issues };
}
