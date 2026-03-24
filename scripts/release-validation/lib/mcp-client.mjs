import { requestJson } from "./http-client.mjs";

export async function mcpCall(apiBase, cookie, payload) {
  return requestJson(apiBase, cookie, "/mcp", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
}

export async function getCapabilities(apiBase, cookie) {
  return requestJson(apiBase, cookie, "/mcp/capabilities");
}

export async function runMcpWorkflow(apiBase, cookie, libraryId) {
  const capabilities = await getCapabilities(apiBase, cookie);
  const initialize = await mcpCall(apiBase, cookie, {
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      clientInfo: { name: "release-validator", version: "1.0.0" },
    },
  });
  const toolsList = await mcpCall(apiBase, cookie, {
    jsonrpc: "2.0",
    id: 2,
    method: "tools/list",
    params: {},
  });
  const searchDocuments = await mcpCall(apiBase, cookie, {
    jsonrpc: "2.0",
    id: 3,
    method: "tools/call",
    params: {
      name: "search_documents",
      arguments: { query: "Acme graph budget", libraryIds: [libraryId], limit: 5 },
    },
  });
  const uploadDocuments = await mcpCall(apiBase, cookie, {
    jsonrpc: "2.0",
    id: 4,
    method: "tools/call",
    params: {
      name: "upload_documents",
      arguments: {
        libraryId,
        documents: [
          {
            fileName: "mcp-release-validation.md",
            mimeType: "text/markdown",
            body: "# MCP workflow\nAcme and Beta validation note in Berlin.",
          },
        ],
      },
    },
  });
  const receiptId = uploadDocuments.data?.result?.structuredContent?.receipts?.[0]?.receiptId ?? null;
  const documentId = uploadDocuments.data?.result?.structuredContent?.receipts?.[0]?.documentId ?? null;

  const mutationStatus = receiptId
    ? await mcpCall(apiBase, cookie, {
        jsonrpc: "2.0",
        id: 5,
        method: "tools/call",
        params: { name: "get_mutation_status", arguments: { receiptId } },
      })
    : null;
  const readDocument = documentId
    ? await mcpCall(apiBase, cookie, {
        jsonrpc: "2.0",
        id: 6,
        method: "tools/call",
        params: { name: "read_document", arguments: { documentId, mode: "full" } },
      })
    : null;

  return {
    capabilities,
    initialize,
    toolsList,
    searchDocuments,
    uploadDocuments,
    mutationStatus,
    readDocument,
  };
}
