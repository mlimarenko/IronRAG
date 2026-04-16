import { beforeEach, describe, expect, it, vi } from "vitest";

import { documentsApi } from "@/api/documents";
import { apiFetch } from "@/api/client";

vi.mock("@/api/client", () => ({
  ApiError: class ApiError extends Error {},
  apiFetch: vi.fn(),
}));

describe("documentsApi.upload", () => {
  beforeEach(() => {
    vi.mocked(apiFetch).mockReset();
    vi.mocked(apiFetch).mockResolvedValue({});
  });

  it("sends canonical external key and relative file name for folder uploads", async () => {
    const file = new File(["demo"], "file.txt", { type: "text/plain" });

    await documentsApi.upload("library-1", file, {
      externalKey: "foo1/path/bar/file.txt",
      fileName: "file.txt",
      title: "foo1/path/bar/file.txt",
    });

    const [, init] = vi.mocked(apiFetch).mock.calls[0] ?? [];
    const form = init?.body;

    expect(form).toBeInstanceOf(FormData);
    expect((form as FormData).get("library_id")).toBe("library-1");
    expect((form as FormData).get("external_key")).toBe(
      "foo1/path/bar/file.txt",
    );
    expect((form as FormData).get("title")).toBe("foo1/path/bar/file.txt");
    expect(((form as FormData).get("file") as File).name).toBe("file.txt");
  });
});
