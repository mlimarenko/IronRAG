import { requestJson } from "./http-client.mjs";

function extractItems(payload) {
  if (!payload) return [];
  if (Array.isArray(payload)) return payload;
  if (Array.isArray(payload.items)) return payload.items;
  if (Array.isArray(payload.documentHits)) return payload.documentHits;
  return [];
}

export async function fetchEntities(apiBase, cookie, libraryId, limit = 200) {
  const response = await requestJson(
    apiBase,
    cookie,
    `/knowledge/libraries/${libraryId}/entities?limit=${limit}`,
  );
  return { ...response, items: extractItems(response.data) };
}

export async function fetchRelations(apiBase, cookie, libraryId, limit = 200) {
  const response = await requestJson(
    apiBase,
    cookie,
    `/knowledge/libraries/${libraryId}/relations?limit=${limit}`,
  );
  return { ...response, items: extractItems(response.data) };
}

export async function searchDocuments(apiBase, cookie, libraryId, query, limit = 5) {
  const response = await requestJson(
    apiBase,
    cookie,
    `/knowledge/libraries/${libraryId}/search/documents?query=${encodeURIComponent(query)}&limit=${limit}`,
  );
  return {
    ...response,
    items: extractItems(response.data?.documentHits ? response.data : response.data?.hits ? response.data : null),
  };
}
