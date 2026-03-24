import fs from "node:fs/promises";

export async function loadJson(path) {
  const raw = await fs.readFile(path, "utf8");
  return JSON.parse(raw);
}

export async function parseJsonResponse(response) {
  const text = await response.text();
  try {
    return {
      status: response.status,
      ok: response.ok,
      data: text ? JSON.parse(text) : null,
      text,
    };
  } catch {
    return {
      status: response.status,
      ok: response.ok,
      data: null,
      text,
    };
  }
}

export async function createSession(apiBase, login, password) {
  const response = await fetch(`${apiBase}/iam/session/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ login, password }),
  });
  const parsed = await parseJsonResponse(response);
  const setCookie = response.headers.get("set-cookie") ?? "";
  const cookie = setCookie.split(";")[0];
  if (!cookie) {
    throw new Error(`login failed: status=${response.status} body=${parsed.text.slice(0, 300)}`);
  }
  return cookie;
}

export async function requestJson(apiBase, cookie, path, options = {}) {
  const headers = {
    ...(options.headers ?? {}),
    cookie,
  };
  const response = await fetch(`${apiBase}${path}`, {
    ...options,
    headers,
  });
  return parseJsonResponse(response);
}
