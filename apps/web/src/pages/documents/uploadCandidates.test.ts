import { describe, expect, it } from "vitest";

import {
  buildUploadCandidates,
  getUploadCandidateName,
  normalizeUploadName,
} from "@/pages/documents/uploadCandidates";

describe("uploadCandidates", () => {
  it("normalizes path separators and trims empty segments", () => {
    expect(normalizeUploadName("  foo\\\\bar///baz.txt  ")).toBe(
      "foo/bar/baz.txt",
    );
  });

  it("uses webkitRelativePath when present", () => {
    const file = new File(["demo"], "baz.txt", { type: "text/plain" });
    Object.defineProperty(file, "webkitRelativePath", {
      value: "foo1/path/bar/baz.txt",
      configurable: true,
    });

    expect(getUploadCandidateName(file)).toBe("foo1/path/bar/baz.txt");
  });

  it("falls back to the plain file name for single-file uploads", () => {
    const file = new File(["demo"], "report.pdf", { type: "application/pdf" });

    expect(buildUploadCandidates([file])).toEqual([
      {
        file,
        name: "report.pdf",
      },
    ]);
  });
});
