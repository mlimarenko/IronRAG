export function getJankTag(longTaskMax) {
  if (longTaskMax > 200) {
    return ' <== SEVERE'
  }

  if (longTaskMax > 80) {
    return ' <- janky'
  }

  return ''
}

export function parseSessionCookie(setCookie) {
  const cookie = setCookie.split(',', 1)[0]
  const separatorIndex = cookie.indexOf('=')

  if (separatorIndex <= 0) {
    return null
  }

  const name = cookie.slice(0, separatorIndex).trim()
  const value = cookie
    .slice(separatorIndex + 1)
    .split(';', 1)[0]
    .trim()

  return name && value ? [name, value] : null
}

function profileFunctionKey(callFrame) {
  const fileName = callFrame.url.split('/').at(-1) || ''
  return `${callFrame.functionName || '(anon)'} @ ${fileName}:${callFrame.lineNumber}`
}

export function summarizeCpuProfile(profile, limit = 14) {
  const nodes = new Map(profile.nodes.map((node) => [node.id, node]))
  const selfTimeByFunction = new Map()

  for (const [index, sample] of (profile.samples || []).entries()) {
    const node = nodes.get(sample)
    if (!node) {
      continue
    }

    const key = profileFunctionKey(node.callFrame)
    const selfTime = selfTimeByFunction.get(key) || 0
    selfTimeByFunction.set(key, selfTime + (profile.timeDeltas?.[index] || 0))
  }

  return [...selfTimeByFunction.entries()]
    .sort(([, left], [, right]) => right - left)
    .slice(0, limit)
}

export function buildBrowserLaunchOptions(browserName, headless, gpuArgs) {
  if (browserName === 'firefox') {
    return { headless }
  }

  return {
    headless,
    args: gpuArgs.split(' ').filter(Boolean),
  }
}
