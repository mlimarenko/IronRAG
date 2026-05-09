export interface UploadCandidate {
  file: File;
  name: string;
}

export function normalizeUploadName(value: string): string {
  return value
    .trim()
    .replace(/\\/g, "/")
    .split("/")
    .map((segment) => segment.trim())
    .filter((segment) => segment.length > 0)
    .join("/");
}

export function getUploadCandidateName(
  file: Pick<File, "name" | "webkitRelativePath">,
): string {
  const relativePath =
    typeof file.webkitRelativePath === "string" ? file.webkitRelativePath : "";
  return normalizeUploadName(relativePath || file.name);
}

export function buildUploadCandidates(files: File[]): UploadCandidate[] {
  return files
    .map((file) => ({ file, name: getUploadCandidateName(file) }))
    .filter((candidate) => candidate.name.length > 0);
}
