import test from "node:test";
import assert from "node:assert/strict";

import { parseJsonResponse } from "../lib/http-client.mjs";

test("parseJsonResponse parses successful JSON body", async () => {
  const response = new Response(
    JSON.stringify({
      ok: true,
      payload: { id: "abc" },
    }),
    {
      status: 200,
      headers: { "content-type": "application/json" },
    },
  );

  const parsed = await parseJsonResponse(response);
  assert.equal(parsed.status, 200);
  assert.equal(parsed.ok, true);
  assert.deepEqual(parsed.data, { ok: true, payload: { id: "abc" } });
  assert.equal(typeof parsed.text, "string");
});

test("parseJsonResponse preserves 404 JSON error envelope", async () => {
  const response = new Response(
    JSON.stringify({
      error: {
        code: "not_found",
        message: "resource does not exist",
      },
    }),
    {
      status: 404,
      headers: { "content-type": "application/json" },
    },
  );

  const parsed = await parseJsonResponse(response);
  assert.equal(parsed.status, 404);
  assert.equal(parsed.ok, false);
  assert.equal(parsed.data.error.code, "not_found");
  assert.equal(parsed.data.error.message, "resource does not exist");
});

test("parseJsonResponse returns null data for non-JSON 500 body", async () => {
  const response = new Response("internal server error", {
    status: 500,
    headers: { "content-type": "text/plain" },
  });

  const parsed = await parseJsonResponse(response);
  assert.equal(parsed.status, 500);
  assert.equal(parsed.ok, false);
  assert.equal(parsed.data, null);
  assert.equal(parsed.text, "internal server error");
});

test("parseJsonResponse handles empty 204 body", async () => {
  const response = new Response(null, { status: 204 });
  const parsed = await parseJsonResponse(response);
  assert.equal(parsed.status, 204);
  assert.equal(parsed.ok, true);
  assert.equal(parsed.data, null);
  assert.equal(parsed.text, "");
});
