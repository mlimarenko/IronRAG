const normalizeBuildVersion = (value: string) => {
  const trimmed = value.trim();
  return trimmed.replace(/^v(?=\d)/, "");
};

export const BUILD_VERSION = normalizeBuildVersion(__APP_VERSION__);
export const BUILD_VERSION_LABEL = `RustRAG v${BUILD_VERSION}`;
export const formatVersionTag = (value: string) => `v${normalizeBuildVersion(value)}`;
