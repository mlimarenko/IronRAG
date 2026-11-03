const normalizeBuildVersion = (value: string) => {
  const trimmed = value.trim()
  return trimmed.replace(/^v(?=\d)/, '')
}

export const formatVersionTag = (value: string) => `v${normalizeBuildVersion(value)}`
export const BUILD_VERSION_TAG = formatVersionTag(__APP_VERSION__)
