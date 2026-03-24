import { fetchEntities, fetchRelations } from "./knowledge-client.mjs";

function itemId(item) {
  return item?.id ?? item?.entity_id ?? item?.relation_id ?? item?.uuid ?? null;
}

function dedupeById(items) {
  const byId = new Map();
  for (const item of items) {
    const id = itemId(item);
    if (id == null) continue;
    if (!byId.has(id)) {
      byId.set(id, item);
    }
  }
  return [...byId.values()];
}

export async function collectGraphSnapshot(apiBase, cookie, libraryId, limit) {
  const [entitiesResponse, relationsResponse] = await Promise.all([
    fetchEntities(apiBase, cookie, libraryId, limit),
    fetchRelations(apiBase, cookie, libraryId, limit),
  ]);

  const entities = dedupeById(Array.isArray(entitiesResponse?.items) ? entitiesResponse.items : []);
  const relations = dedupeById(Array.isArray(relationsResponse?.items) ? relationsResponse.items : []);

  return {
    entities,
    relations,
    entityCount: entities.length,
    relationCount: relations.length,
  };
}
