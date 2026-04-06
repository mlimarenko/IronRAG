const API_BASE = "/v1";

export class ApiError extends Error {
  constructor(public status: number, public body: any) {
    super(body?.error || `API error ${status}`);
  }
}

export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const mergedHeaders: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (init?.headers) {
    const h = init.headers as Record<string, string>;
    for (const [k, v] of Object.entries(h)) {
      mergedHeaders[k] = v;
    }
  }
  const res = await fetch(`${API_BASE}${path}`, {
    credentials: "include",
    method: init?.method,
    body: init?.body,
    headers: mergedHeaders,
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}
